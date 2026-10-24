//! Whole unchanged host deployment comparison. No engine, crypto or host proof.
use super::super::control::Window;
use super::*;
use crate::test_support::Fixture;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Read},
    os::{
        fd::AsFd,
        unix::fs::{FileExt, MetadataExt},
    },
    path::{Path, PathBuf},
};
const FILE_BYTES: u64 = 4 * 1024 * 1024;
const WHOLE: Duration = Duration::from_secs(1800);
const RESERVE: Duration = Duration::from_secs(60);
const PYTHON_BODY: Duration = Duration::from_secs(70);
const PYTHON_TAIL: Duration = Duration::from_secs(10);

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Oracle {
    schema_version: u8,
    scenario: String,
    input: Value,
    observation: Value,
}

fn read(path: &Path) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let result = (|| {
        if file.metadata()?.len() > FILE_BYTES {
            return Err(io::Error::other("oracle file exceeds its fixture bound"));
        }
        let mut data = Vec::new();
        (&file).take(FILE_BYTES + 1).read_to_end(&mut data)?;
        if data.len() as u64 > FILE_BYTES {
            return Err(io::Error::other(
                "oracle file grew beyond its fixture bound",
            ));
        }
        Ok(data)
    })();
    let closed = nix::unistd::close(file).map_err(io::Error::from);
    result.and_then(|data| closed.map(|()| data))
}

fn hash(data: &[u8]) -> String {
    super::super::files::hex(&Sha256::digest(data))
}

fn normalize(mut value: Value, root: &Path) -> Value {
    match &mut value {
        Value::Object(map) => {
            for (key, value) in map {
                *value = if key == "quietWaitSeconds" && value.is_number() {
                    json!(value.as_f64().unwrap())
                } else if matches!(
                    key.as_str(),
                    "startedAt"
                        | "finishedAt"
                        | "stagedAt"
                        | "completedAt"
                        | "interruptionStartedAt"
                        | "interruptionFinishedAt"
                        | "roomsObservedBeforeStopAt"
                ) && value.is_string()
                {
                    Value::String("$UTC".into())
                } else {
                    normalize(value.take(), root)
                };
            }
        }
        Value::Array(values) => {
            for value in values {
                *value = normalize(value.take(), root);
            }
        }
        Value::String(text) => {
            *text = text
                .replace(root.join("public").to_str().unwrap(), "$PUBLIC")
                .replace(root.join("config").to_str().unwrap(), "$CONFIG")
                .replace(root.join("work").to_str().unwrap(), "$WORK")
                .replace(root.to_str().unwrap(), "$FIXTURE");
            if let Some(start) = text.find("/results/release.") {
                let first = start + "/results/".len();
                let end = text[first..]
                    .find('/')
                    .map_or(text.len(), |count| first + count);
                text.replace_range(first..end, "$ATTEMPT");
            }
            for (prefix, replacement) in [
                ("scpub-release-validate-", "$VALIDATOR"),
                ("scpub-release-assets-", "$ASSET_NAME"),
            ] {
                if let Some(at) = text.find(prefix) {
                    let end = at + prefix.len() + 32;
                    if text.len() >= end
                        && text[at + prefix.len()..end]
                            .bytes()
                            .all(|byte| byte.is_ascii_hexdigit())
                    {
                        text.replace_range(at..end, replacement);
                    }
                }
            }
        }
        _ => {}
    }
    value
}

fn tree(path: &Path, root: &Path) -> io::Result<Value> {
    fn visit(
        path: &Path,
        base: &Path,
        root: &Path,
        found: &mut BTreeMap<String, Value>,
    ) -> io::Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let mut entries = fs::read_dir(path)?.collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let name = path
                .strip_prefix(base)
                .unwrap()
                .to_str()
                .unwrap()
                .replace('\\', "/");
            let metadata = fs::symlink_metadata(&path)?;
            let mode = metadata.mode() & 0o7777;
            let item = if metadata.file_type().is_symlink() {
                json!({"symlink":fs::read_link(&path)?.to_str().unwrap(),"mode":mode})
            } else if metadata.is_dir() {
                visit(&path, base, root, found)?;
                json!({"directory":true,"mode":mode})
            } else if metadata.is_file() {
                let data = read(&path)?;
                let mut item = json!({"sha256":hash(&data),"bytes":data.len(),"mode":mode});
                if path.extension().is_some_and(|v| v == "json") {
                    let maximum = usize::try_from(FILE_BYTES).map_err(io::Error::other)?;
                    let semantic = crate::json::decode_limited(&data, maximum)
                        .ok()
                        .and_then(|_| serde_json::from_slice(&data).ok());
                    match semantic {
                        Some(value) => item["value"] = normalize(value, root),
                        None => {
                            item["text"] = String::from_utf8(data)
                                .map(Value::String)
                                .map_err(|_| io::Error::other("invalid oracle record text"))?
                        }
                    }
                } else if matches!(
                    path.file_name().and_then(|v| v.to_str()),
                    Some("validation-name.txt" | "asset-container-name.txt")
                ) {
                    item["text"] = normalize(
                        Value::String(
                            String::from_utf8(data)
                                .map_err(|_| io::Error::other("invalid oracle name text"))?,
                        ),
                        root,
                    );
                }
                item
            } else {
                return Err(io::Error::other("unexpected oracle file kind"));
            };
            found.insert(name, item);
        }
        Ok(())
    }
    let mut found = BTreeMap::new();
    visit(path, path, root, &mut found)?;
    serde_json::to_value(found).map_err(io::Error::other)
}

fn semantic_tree(mut tree: Value) -> Value {
    for item in tree.as_object_mut().unwrap().values_mut() {
        if let Some(map) = item.as_object_mut()
            && map.contains_key("value")
        {
            // Keep original SHA/count observations separately. New complete JSON
            // has a fixed semantic contract; writer formatting is not parity.
            map.remove("sha256");
            map.remove("bytes");
        }
    }
    tree
}

fn command_aliases(mut arguments: Value) -> Value {
    if let Some(first) = arguments
        .as_array_mut()
        .and_then(|values| values.first_mut())
    {
        *first = match first.as_str() {
            Some("/usr/bin/curl" | "$FIXTURE/bin/curl") => Value::String("$CURL".into()),
            Some("$FIXTURE/bin/docker") => Value::String("$DOCKER".into()),
            _ => first.take(),
        };
    }
    arguments
}

fn record(path: &Path, root: &Path) -> io::Result<Value> {
    let data = read(path)?;
    crate::json::decode_limited(&data, FILE_BYTES as usize)
        .map_err(|_| io::Error::other("invalid oracle record"))?;
    serde_json::from_slice::<Value>(&data)
        .map(|value| normalize(value, root))
        .map_err(io::Error::other)
}

fn lock_available(path: &Path) -> io::Result<bool> {
    let file = File::options().read(true).write(true).open(path)?;
    let available = rustix::fs::flock(
        file.as_fd(),
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .is_ok();
    nix::unistd::close(file)
        .map_err(io::Error::from)
        .map(|()| available)
}

fn command_state(root: &Path) -> io::Result<Value> {
    let mut selection = serde_json::Map::new();
    for name in ["compose.public.yml", "app.env", "images.json"] {
        let path = root.join("config").join(name);
        let data = read(&path)?;
        let metadata = fs::symlink_metadata(&path)?;
        let value = if name == "images.json"
            && crate::json::decode_limited(&data, FILE_BYTES as usize).is_ok()
        {
            serde_json::from_slice(&data).map_err(io::Error::other)?
        } else {
            Value::String(String::from_utf8(data).map_err(io::Error::other)?)
        };
        selection.insert(
            name.into(),
            json!({"mode":metadata.mode() & 0o7777,
            "value":normalize(value,root)}),
        );
    }
    let mut attempts = fs::read_dir(root.join("public/results"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    attempts.retain(|path| {
        path.file_name().is_some_and(|name| {
            name.to_str()
                .is_some_and(|name| name.starts_with("release."))
        })
    });
    if attempts.len() != 1 {
        return Err(io::Error::other("nonunique live oracle attempt"));
    }
    let attempt = &attempts[0];
    let dump = attempt.join("database-before.dump");
    let backup = if dump.exists() {
        let data = read(&dump)?;
        json!({"sha256":hash(&data),"bytes":data.len(),
            "mode":fs::symlink_metadata(&dump)?.mode() & 0o7777})
    } else {
        Value::Null
    };
    let optional = |path: PathBuf| -> io::Result<Value> {
        if path.exists() {
            record(&path, root)
        } else {
            Ok(Value::Null)
        }
    };
    Ok(json!({"selection":selection,
        "journal":optional(root.join("public/release-state.json"))?,
        "assetVersion":optional(root.join("public/browser-assets/public/app-version.json"))?,
        "backup":backup,"backupReceipt":optional(attempt.join("database-before.receipt.json"))?,
        "lockHeld":!lock_available(&root.join("work/workload.lock"))?}))
}

struct Replay {
    root: PathBuf,
    reference: PathBuf,
    expected: Vec<Value>,
    seen: Vec<Value>,
    mismatch: Option<&'static str>,
    scenario: String,
    completion_fault: bool,
}
impl Executor for Replay {
    async fn run(&mut self, owner: &mut Owner, request: Invocation<'_>) -> CommandOutput {
        let index = self.seen.len();
        let args = std::iter::once(request.program.as_os_str())
            .chain(request.arguments.iter().map(OsString::as_os_str))
            .map(|value| Value::String(value.to_str().unwrap().to_owned()))
            .collect::<Vec<_>>();
        let actual = command_aliases(normalize(Value::Array(args), &self.root));
        self.seen.push(actual.clone());
        let mut error = None;
        let expected = self.expected.get(index).cloned().unwrap_or(Value::Null);
        let state = command_state(&self.root);
        let expected_state = normalize(expected["stateAtStart"].clone(), &self.reference);
        if state.as_ref().ok() != Some(&expected_state) {
            self.mismatch
                .get_or_insert("filesystem chronology differs at command entry");
            error.get_or_insert(Cause::Changed);
        }
        if error.is_some()
            || command_aliases(normalize(expected["argv"].clone(), &self.reference)) != actual
        {
            self.mismatch
                .get_or_insert("fixed command argument/order mismatch");
            return CommandOutput {
                stdout: Vec::new(),
                stderr: Vec::new(),
                status: None,
                error: Some(error.unwrap_or(Cause::Policy)),
                settled: true,
                stdout_complete: false,
                bulk_bytes: 0,
                bulk_sha256: hash(b""),
                bulk_complete: false,
            };
        }
        let mut load = |label: &str| -> Vec<u8> {
            let field = format!("{label}File");
            let Some(name) = expected.get(&field).and_then(Value::as_str) else {
                self.mismatch
                    .get_or_insert("missing original command capture");
                return Vec::new();
            };
            if !name.starts_with("responses/") || name.contains("..") {
                self.mismatch
                    .get_or_insert("unexpected original capture selector");
                return Vec::new();
            }
            match read(&self.reference.join(name)) {
                Ok(data) => {
                    let pin = &expected[format!("{label}Pin")];
                    if pin["sha256"].as_str() != Some(hash(&data).as_str())
                        || pin["bytes"].as_u64() != Some(data.len() as u64)
                    {
                        self.mismatch.get_or_insert("original capture changed");
                    }
                    data
                }
                Err(_) => {
                    self.mismatch.get_or_insert("original capture unavailable");
                    Vec::new()
                }
            }
        };
        let mut data = load("stdout");
        let stderr = load("stderr");
        // Only complete structured external JSON has fixture filesystem spelling.
        // Its original byte pins were verified above; payloads/TARs remain raw.
        if crate::json::decode_limited(&data, FILE_BYTES as usize).is_ok()
            && let Ok(text) = std::str::from_utf8(&data)
        {
            data = text
                .replace(
                    self.reference.to_str().unwrap(),
                    self.root.to_str().unwrap(),
                )
                .into_bytes();
        }
        if !expected["inputPath"].is_null() {
            if let Some(input) = request.input {
                let mut source = Vec::new();
                let mut buffer = vec![0; 65536];
                let mut offset = 0_u64;
                loop {
                    if let Err(cause) = owner.check(Window::Body) {
                        error.get_or_insert(cause);
                        break;
                    }
                    let result = input.read_at(&mut buffer, offset);
                    if result.is_err() {
                        owner.record(Cause::Io);
                        error.get_or_insert(Cause::Io);
                    }
                    if let Err(cause) = owner.check(Window::Body) {
                        error.get_or_insert(cause);
                        break;
                    }
                    let Ok(count) = result else {
                        error.get_or_insert(Cause::Io);
                        break;
                    };
                    if count == 0 {
                        break;
                    }
                    offset += count as u64;
                    if offset > FILE_BYTES {
                        error.get_or_insert(Cause::Bounds);
                        break;
                    }
                    source.extend_from_slice(&buffer[..count]);
                }
                let original = read(&self.reference.join("dump.bin"));
                if original.as_deref().ok() != Some(source.as_slice()) {
                    self.mismatch
                        .get_or_insert("original inherited backup input differs");
                    error.get_or_insert(Cause::Changed);
                }
            } else {
                self.mismatch
                    .get_or_insert("missing original inherited input descriptor");
                error.get_or_insert(Cause::Input);
            }
        } else if request.input.is_some() {
            self.mismatch
                .get_or_insert("unexpected inherited input descriptor");
            error.get_or_insert(Cause::Input);
        }
        let mut bulk_bytes = 0;
        let mut bulk_complete = request.output.is_none();
        let mut bulk_sha256 = hash(b"");
        let stdout = if let Some(bulk) = request.output {
            for chunk in data.chunks(65536) {
                if bulk_bytes + chunk.len() as u64 > bulk.maximum {
                    error.get_or_insert(Cause::Bounds);
                    break;
                }
                if let Err(cause) = owner.check(Window::Body) {
                    error.get_or_insert(cause);
                    break;
                }
                let mut rest = chunk;
                while !rest.is_empty() {
                    match bulk.file.write_at(rest, bulk_bytes) {
                        Ok(0) | Err(_) => {
                            owner.record(Cause::Io);
                            error.get_or_insert(Cause::Io);
                            break;
                        }
                        Ok(count) => {
                            bulk_bytes += count as u64;
                            rest = &rest[count..];
                        }
                    }
                }
                if error.is_some() {
                    break;
                }
            }
            bulk_complete = bulk_bytes == data.len() as u64 && error.is_none();
            bulk_sha256 = hash(&data[..bulk_bytes as usize]);
            Vec::new()
        } else {
            data
        };
        let status = expected["status"]
            .as_i64()
            .and_then(|value| i32::try_from(value).ok());
        if !self.completion_fault
            && actual
                .as_array()
                .is_some_and(|args| args.iter().any(|value| value == "up"))
            && matches!(
                self.scenario.as_str(),
                "candidate-up-close"
                    | "candidate-up-close-rollback-up"
                    | "candidate-up-late-result"
            )
        {
            self.completion_fault = true;
            error.get_or_insert(if self.scenario == "candidate-up-late-result" {
                Cause::Deadline
            } else {
                Cause::Close
            });
        }
        CommandOutput {
            stdout,
            stderr,
            status,
            error,
            settled: error != Some(Cause::Close),
            stdout_complete: true,
            bulk_bytes,
            bulk_sha256,
            bulk_complete,
        }
    }
}

const SCENARIOS: &[&str] = &[
    "success",
    "prestaged",
    "received-hardlinks",
    "env-no-final-lf",
    "compose-crlf",
    "caddy-public-0644",
    "restore-without-read",
    "root-mode",
    "config-mode",
    "lock-busy",
    "current-unfinished",
    "release-unfinished",
    "archive-digest",
    "staged-manifest-diff",
    "images-syntax",
    "old-image-invalid",
    "old-revision-invalid",
    "already-selected",
    "old-image-label",
    "app-not-running",
    "app-image-drift",
    "app-config-drift",
    "db-config-drift",
    "proxy-config-drift",
    "db-unhealthy",
    "migrate-retained",
    "pre-loop-unready",
    "pre-public-unready",
    "ledger-missing",
    "ledger-extra",
    "ledger-checksum",
    "ledger-failed",
    "ledger-duplicate",
    "ledger-empty",
    "packaged-mismatch",
    "preview-unrelated",
    "runtime-loader",
    "compose-image-count",
    "env-image-duplicate",
    "legacy-assets",
    "asset-receipt-diff",
    "asset-mount-writable",
    "assets-old-create",
    "assets-old-copy",
    "assets-old-invalid",
    "assets-old-remove",
    "assets-new-copy",
    "assets-create-unknown",
    "quiet-no-token",
    "quiet-unavailable",
    "quiet-malformed",
    "quiet-active-zero",
    "quiet-active-deadline",
    "rooms-arrive-after-quiet",
    "db-size-invalid",
    "backup-disk-exact",
    "backup-disk-below",
    "dump-empty",
    "dump-nonzero",
    "dump-partial",
    "dump-above-inspection",
    "list-nonzero",
    "backup-file-sync",
    "backup-rename",
    "backup-directory-sync",
    "backup-receipt-write",
    "before-copy",
    "replace-journal-write",
    "replacement-observation-time",
    "app-stop",
    "select-compose-write",
    "select-env-write",
    "select-images-write",
    "select-assets",
    "candidate-up",
    "candidate-up-close",
    "candidate-up-close-rollback-up",
    "candidate-up-late-result",
    "candidate-wrong-image",
    "candidate-loop-unready",
    "candidate-public-unready",
    "database-restarted",
    "proxy-restarted",
    "rollback-stop",
    "rollback-compose-write",
    "rollback-assets",
    "rollback-up",
    "rollback-wrong-image",
    "rollback-loop-unready",
    "rollback-public-unready",
    "rollback-dependency",
    "complete-journal-write",
    "outcome-write",
    "outcome-file-sync",
    "outcome-directory-sync",
    "interrupt-before-stop",
    "interrupt-after-stop",
];

const BASELINE_PYTHON_PATHS: &[&str] = &[
    "build/bootstrap.py",
    "build/bootstrap_access.py",
    "build/bootstrap_controller.py",
    "build/build-release.py",
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
        "xtask/src/ci/release_http_policy/reference/ops/ansible/tests/",
        "test_release_container_harness.py"
    ),
    "xtask/src/ci/release_http_policy/reference/ops/ansible/tests/test_release_http_policy.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/tests/test_support.py",
];

fn source_pins(repository: &Path) -> io::Result<Value> {
    let mut pins = serde_json::Map::new();
    for name in BASELINE_PYTHON_PATHS {
        let path = repository.join(name);
        let data = read(&path)?;
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file() {
            return Err(io::Error::other(
                "original oracle source is not a regular file",
            ));
        }
        pins.insert(
            (*name).into(),
            json!({"sha256":hash(&data),"bytes":data.len(),
            "mode":metadata.mode() & 0o7777}),
        );
    }
    Ok(Value::Object(pins))
}

fn identities(path: &Path) -> io::Result<Value> {
    fn visit(
        path: &Path,
        base: &Path,
        result: &mut serde_json::Map<String, Value>,
    ) -> io::Result<()> {
        for entry in fs::read_dir(path)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                visit(&path, base, result)?;
            } else if metadata.is_file() {
                let name = path.strip_prefix(base).unwrap().to_str().unwrap();
                result.insert(
                    name.into(),
                    json!({"dev":metadata.dev(),"ino":metadata.ino(),
                    "uid":metadata.uid(),"gid":metadata.gid(),"nlink":metadata.nlink(),
                    "mode":metadata.mode(),"bytes":metadata.len(),
                    "mtimeNs":i128::from(metadata.mtime())*1_000_000_000+
                        i128::from(metadata.mtime_nsec())}),
                );
            }
        }
        Ok(())
    }
    let mut result = serde_json::Map::new();
    visit(path, path, &mut result)?;
    Ok(Value::Object(result))
}

fn fault(scenario: &str) -> Option<files::Fault> {
    Some(match scenario {
        "backup-file-sync" => files::Fault::BackupFileSync,
        "backup-rename" => files::Fault::BackupRename,
        "backup-directory-sync" => files::Fault::BackupDirectorySync,
        "backup-receipt-write" => files::Fault::BackupReceiptWrite,
        "before-copy" => files::Fault::BeforeCopy,
        "replace-journal-write" => files::Fault::ReplaceJournalWrite,
        "select-compose-write" => files::Fault::SelectComposeWrite,
        "select-env-write" => files::Fault::SelectEnvWrite,
        "select-images-write" => files::Fault::SelectImagesWrite,
        "select-assets" => files::Fault::SelectAssets,
        "rollback-compose-write" => files::Fault::RollbackComposeWrite,
        "rollback-assets" => files::Fault::RollbackAssets,
        "complete-journal-write" => files::Fault::CompleteJournalWrite,
        "outcome-write" => files::Fault::OutcomeWrite,
        "outcome-file-sync" => files::Fault::OutcomeFileSync,
        "outcome-directory-sync" => files::Fault::OutcomeDirectorySync,
        "interrupt-before-stop" => files::Fault::InterruptBeforeStop,
        "interrupt-after-stop" => files::Fault::InterruptAfterStop,
        "replacement-observation-time" => files::Fault::ReplacementObservationTime,
        _ => return None,
    })
}

fn unknown_asset_profile(scenario: &str) -> bool {
    matches!(scenario, "assets-old-create" | "assets-create-unknown")
}

fn failure_profile(scenario: &str) -> Option<(&'static str, &'static str)> {
    match scenario {
        "archive-digest" => Some((
            "native_archive_refused",
            "Image archive SHA256 does not match release manifest",
        )),
        "assets-old-invalid" => Some(("asset_tar_path", "AssetError")),
        "candidate-up-close" | "candidate-up-close-rollback-up" => {
            Some(("native_close_refused", "OSError"))
        }
        _ => None,
    }
}

fn failure_projection(scenario: &str, native: &Value, original: &Value) -> io::Result<Value> {
    let Some((native_code, original_message)) = failure_profile(scenario) else {
        return Err(io::Error::other("unknown named diagnostic profile"));
    };
    if native["failure"] != native_code || original["failure"] != original_message {
        return Err(io::Error::other(
            "unexpected named diagnostic message or code",
        ));
    }
    // These four exact rows preserve typed Stage/Assets/Close diagnostics.
    // Original reports, raw pins and traces stay intact; every other key is strict.
    let mut projected = native.clone();
    projected["failure"] = original["failure"].clone();
    Ok(projected)
}

#[test]
fn named_diagnostic_profiles_require_each_exact_case_message_and_code() {
    for scenario in [
        "archive-digest",
        "assets-old-invalid",
        "candidate-up-close",
        "candidate-up-close-rollback-up",
    ] {
        let (native_code, original_message) = failure_profile(scenario).unwrap();
        let native = json!({"failure":native_code,"passed":false,"other":"strict"});
        let original = json!({"failure":original_message,"passed":false,"other":"strict"});
        assert_eq!(
            failure_projection(scenario, &native, &original).unwrap(),
            original
        );
        assert_eq!(native["failure"], native_code);
        for (name, native, original) in [
            ("another-case", native.clone(), original.clone()),
            (
                scenario,
                json!({"failure":"another native code"}),
                original.clone(),
            ),
            (
                scenario,
                native.clone(),
                json!({"failure":"another original failure"}),
            ),
        ] {
            assert!(failure_projection(name, &native, &original).is_err());
        }
    }
}

fn canonical_call_list(commands: &[Value]) -> Vec<Value> {
    commands
        .iter()
        .map(|command| command_aliases(command["argv"].clone()))
        .collect()
}

fn comparison_tree(
    path: &Path,
    root: &Path,
    reference: &Oracle,
    native: bool,
) -> io::Result<Value> {
    let mut result = tree(path, root)?;
    for (name, item) in result.as_object_mut().unwrap() {
        let file = path.join(name);
        let leaf = file
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let Some(fields) = item.as_object_mut() else {
            continue;
        };
        if let Some(prefix) = match leaf {
            "validation-name.txt" => Some("scpub-release-validate-"),
            "asset-container-name.txt" => Some("scpub-release-assets-"),
            _ => None,
        } {
            let data = read(&file)?;
            let text = std::str::from_utf8(&data).map_err(io::Error::other)?;
            let nonce = text
                .strip_prefix(prefix)
                .ok_or_else(|| io::Error::other("wrong closed generated-name prefix"))?;
            if nonce.len() != 32
                || !nonce
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                || fields.get("sha256") != Some(&json!(hash(&data)))
                || fields.get("bytes") != Some(&json!(data.len()))
                || fields.get("text") != Some(&normalize(Value::String(text.into()), root))
            {
                return Err(io::Error::other(
                    "incomplete or changed generated-name record",
                ));
            }
            // Only these two complete closed nonce records have cross-world
            // entropy. Actual raw pins remain in both captured observations;
            // byte count/mode/text and every other file/frame stay compared.
            fields.remove("sha256");
        }
        if matches!(
            leaf,
            "compose.public.yml"
                | "before-compose.public.yml"
                | "candidate-compose.yml"
                | "reviewed-compose.yml"
        ) {
            let data = read(&file)?;
            let text = String::from_utf8(data).map_err(io::Error::other)?;
            fields.insert("ownedPathText".into(), normalize(Value::String(text), root));
            fields.remove("sha256");
            fields.remove("bytes");
        }
        if let Some(number) = leaf
            .strip_suffix(".stdout")
            .and_then(|name| name.parse::<usize>().ok())
            && number > 0
            && let Some(role) = reference.observation["daemon"]["calls"]
                .get(number - 1)
                .and_then(|call| call["role"].as_str())
            && matches!(
                role,
                "image-inspect"
                    | "container-inspect"
                    | "compose-config"
                    | "compose-preview"
                    | "loop-pre-ready"
                    | "public-pre-ready"
                    | "loop-candidate-ready"
                    | "public-candidate-ready"
                    | "loop-rollback-ready"
                    | "public-rollback-ready"
            )
        {
            let data = read(&file)?;
            if crate::json::decode_limited(&data, FILE_BYTES as usize).is_ok() {
                let value = serde_json::from_slice(&data).map_err(io::Error::other)?;
                fields.insert("commandJson".into(), normalize(value, root));
                fields.remove("sha256");
                fields.remove("bytes");
            }
        }
        if reference.scenario == "outcome-file-sync" && leaf.starts_with(".release-") {
            let data = read(&file)?;
            let value = crate::json::decode_limited(&data, FILE_BYTES as usize)
                .ok()
                .and_then(|_| serde_json::from_slice::<Value>(&data).ok());
            if let Some(value) = value {
                let framed = if native {
                    let mut expected =
                        serde_json::to_vec_pretty(&value).map_err(io::Error::other)?;
                    expected.push(b'\n');
                    expected == data
                } else {
                    reference.observation["atomicIntended"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|intent| {
                            intent["path"]
                                .as_str()
                                .is_some_and(|name| name.ends_with("/outcome.json"))
                                && intent["sha256"].as_str() == Some(hash(&data).as_str())
                                && intent["bytes"].as_u64() == Some(data.len() as u64)
                        })
                };
                if framed
                    && value["action"] == "deploy"
                    && value["passed"] == true
                    && value["phase"] == "complete"
                    && value["revision"] == reference.input["revision"]
                {
                    fields.insert("completeOwnFrame".into(), true.into());
                    fields.insert("value".into(), normalize(value, root));
                    fields.remove("sha256");
                    fields.remove("bytes");
                }
            }
        }
    }
    let mut renamed = serde_json::Map::new();
    for (name, value) in std::mem::take(result.as_object_mut().unwrap()) {
        let name = if value["completeOwnFrame"] == true {
            "$COMPLETE_OUTCOME_TMP".into()
        } else {
            name
        };
        if renamed.insert(name, value).is_some() {
            return Err(io::Error::other(
                "distinct retained temporary frames conflated",
            ));
        }
    }
    Ok(semantic_tree(Value::Object(renamed)))
}

struct NativeCase {
    passed: bool,
    reports: Vec<Value>,
    primary: Option<Cause>,
    inputs_before: Value,
    inputs_after: Value,
    identities_before: Value,
    identities_after: Value,
    attempts: Vec<Value>,
    release: Value,
    config: Value,
    assets: Value,
    state: Value,
    calls: Vec<Value>,
    mismatch: Option<&'static str>,
    shared: (usize, usize),
    records: (usize, usize),
    lock_released: bool,
}

fn attempt_paths(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(root.join("public/results"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    paths.retain(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("release."))
    });
    paths.sort();
    Ok(paths)
}

async fn native_case(
    reference: &Oracle,
    root: &Path,
    owner: &mut Owner,
    started: Instant,
) -> io::Result<NativeCase> {
    let source = root.parent().unwrap().join("reference");
    let before = tree(&root.join("public/releases"), root)?;
    let identity_before = identities(&root.join("public/releases"))?;
    let revision = reference.input["revision"]
        .as_str()
        .ok_or_else(|| io::Error::other("oracle revision selector"))?
        .to_owned();
    let quiet_seconds = reference.input["quietSeconds"]
        .as_u64()
        .ok_or_else(|| io::Error::other("oracle quiet selector"))?;
    let expected = reference.observation["commands"]
        .as_array()
        .ok_or_else(|| io::Error::other("oracle command observations"))?
        .clone();
    let uid = fs::metadata(root)?.uid();
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
    let options = Options {
        revision,
        quiet_seconds,
        whole: WHOLE,
        reserve: RESERVE,
    };
    let access = Access {
        root: root.join("public"),
        config: root.join("config"),
        work: root.join("work"),
        docker: root.join("bin/docker"),
        curl: root.join("bin/curl"),
        socket: None,
        uid,
        free_bytes: reference.input["freeBytes"].as_u64(),
        wall_seconds: Some(2524608000),
        fault: fault(&reference.scenario),
        virtual_poll: true,
    };
    let mut replay = Replay {
        root: root.into(),
        reference: source,
        expected,
        seen: Vec::new(),
        mismatch: None,
        scenario: reference.scenario.clone(),
        completion_fault: false,
    };
    super::super::files::start_probe(None);
    files::start_probe();
    // Preserve real fixture and producer time. This is the only virtual-clock
    // initialization; every later change advances this same original owner.
    owner.elapsed = Some(started.elapsed());
    let result = execute(&options, &access, owner, &mut replay, started).await;
    let held_closed = held
        .map(|file| nix::unistd::close(file).map_err(io::Error::from))
        .transpose();
    let shared = super::super::files::take_probe();
    let records = files::take_probe();
    held_closed?;
    let reports = if result.bytes.is_empty() {
        Vec::new()
    } else {
        let value = serde_json::from_slice(&result.bytes).map_err(io::Error::other)?;
        vec![normalize(value, root)]
    };
    let release = tree(&root.join("public/releases"), root)?;
    let after = Value::Object(
        before
            .as_object()
            .unwrap()
            .keys()
            .map(|name| {
                (
                    name.clone(),
                    release.get(name).cloned().unwrap_or(Value::Null),
                )
            })
            .collect(),
    );
    let all_identities = identities(&root.join("public/releases"))?;
    let identity_after = Value::Object(
        identity_before
            .as_object()
            .unwrap()
            .keys()
            .map(|name| {
                (
                    name.clone(),
                    all_identities.get(name).cloned().unwrap_or(Value::Null),
                )
            })
            .collect(),
    );
    let state = root.join("public/release-state.json");
    Ok(NativeCase {
        passed: result.passed,
        reports,
        primary: owner.error(),
        inputs_before: before,
        inputs_after: after,
        identities_before: identity_before,
        identities_after: identity_after,
        attempts: attempt_paths(root)?
            .iter()
            .map(|path| tree(path, root))
            .collect::<io::Result<Vec<_>>>()?,
        release,
        config: tree(&root.join("config"), root)?,
        assets: tree(&root.join("public/browser-assets"), root)?,
        state: if state.exists() {
            record(&state, root)?
        } else {
            Value::Null
        },
        calls: replay.seen,
        mismatch: replay.mismatch,
        shared,
        records,
        lock_released: lock_available(&root.join("work/workload.lock"))?,
    })
}

fn compare(reference: &Oracle, native: &NativeCase, root: &Path, finished: bool) {
    let name = &reference.scenario;
    let observed = &reference.observation;
    assert_eq!(reference.schema_version, 1);
    assert_eq!(
        native.mismatch, None,
        "{name}: lower effect chronology/argv"
    );
    assert_eq!(
        native.inputs_before, native.inputs_after,
        "{name}: read-only input bytes/modes"
    );
    assert_eq!(
        native.identities_before, native.identities_after,
        "{name}: original inode generations"
    );
    assert_eq!(observed["inputPinsBefore"], observed["inputPinsAfter"]);
    assert_eq!(
        observed["inputIdentityBefore"],
        observed["inputIdentityAfter"]
    );
    assert!(
        native.lock_released,
        "{name}: actual workload lock retirement"
    );
    assert!(
        native.shared.0 > 0,
        "{name}: actual shared original close observations"
    );
    assert_eq!(native.shared.1, 0, "{name}: no shared fallback-as-positive");
    assert_eq!(
        native.records.1, 0,
        "{name}: no atomic fallback-as-positive"
    );
    if !native.attempts.is_empty() {
        assert!(native.records.0 > 0);
    }
    let expected_pass = observed["exitCodes"] == json!([0]);
    assert_eq!(
        native.passed && finished,
        expected_pass,
        "{name}: complete final native verdict"
    );
    if !expected_pass {
        assert!(
            native.primary.is_some(),
            "{name}: first typed cause retained"
        );
    }
    for key in [
        "sourceUnchanged",
        "originalBaselinePythonUnchanged",
        "originalPythonUnchanged",
        "lockReleased",
    ] {
        assert_eq!(observed[key], true, "{name}: original {key}");
    }
    assert_eq!(
        observed["workerSettled"],
        json!({"status":0,"reaped":true,
        "pipesClosed":true,"stderrBytes":0})
    );
    for child in observed["children"].as_array().unwrap() {
        assert!(child["status"].as_i64().is_some());
        assert_eq!(child["pipesClosed"], true);
        assert_eq!(child["logsClosed"], true);
    }
    let mut expected = canonical_call_list(observed["commands"].as_array().unwrap());
    for call in &mut expected {
        *call = normalize(call.take(), &root.join("reference"));
    }
    if unknown_asset_profile(name) {
        let omitted = expected.pop().unwrap();
        assert_eq!(omitted[0], "$DOCKER");
        assert_eq!(omitted[3], "rm");
        assert_eq!(omitted[4], "$ASSET_NAME");
        assert!(
            observed["daemon"]["containers"]
                .as_object()
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(
        native.calls, expected,
        "{name}: complete fixed effect order"
    );
    let reports = normalize(observed["reports"].clone(), &root.join("reference"));
    let mut native_reports = native.reports.clone();
    if matches!(
        name.as_str(),
        "candidate-up-close" | "candidate-up-close-rollback-up"
    ) {
        assert_eq!(
            native.primary,
            Some(Cause::Close),
            "{name}: typed first cause"
        );
        assert_eq!(
            observed["faultObserved"], true,
            "{name}: actual reference fault"
        );
    }
    if failure_profile(name).is_some() {
        assert_eq!(native_reports.len(), 1);
        assert_eq!(reports.as_array().unwrap().len(), 1);
        native_reports[0] = failure_projection(name, &native_reports[0], &reports[0]).unwrap();
    }
    assert_eq!(
        Value::Array(native_reports),
        reports,
        "{name}: complete passive reports"
    );
    assert_eq!(
        native.state,
        normalize(observed["releaseState"].clone(), &root.join("reference")),
        "{name}: exact persistent journal"
    );
    assert_eq!(
        semantic_tree(native.release.clone()),
        semantic_tree(normalize(
            observed["releaseFiles"].clone(),
            &root.join("reference")
        ))
    );
    // Cross-writer/owned-path projections for attempts/config/assets are made
    // from independently remeasured retained source files, never a report flag.
    assert_eq!(
        native.config,
        tree(&root.join("native/config"), &root.join("native")).unwrap(),
        "{name}: captured configuration remains unchanged through runtime retirement"
    );
    let reference_root = root.join("reference");
    let expected_paths = attempt_paths(&reference_root).unwrap();
    let actual_paths = attempt_paths(&root.join("native")).unwrap();
    assert_eq!(
        native.attempts.len(),
        expected_paths.len(),
        "{name}: all attempts retained"
    );
    for (actual_path, expected_path) in actual_paths.iter().zip(expected_paths.iter()) {
        let mut actual =
            comparison_tree(actual_path, &root.join("native"), reference, true).unwrap();
        let mut expected =
            comparison_tree(expected_path, &reference_root, reference, false).unwrap();
        if failure_profile(name).is_some() {
            actual["outcome.json"]["value"] = failure_projection(
                name,
                &actual["outcome.json"]["value"],
                &expected["outcome.json"]["value"],
            )
            .unwrap();
        }
        if unknown_asset_profile(name) {
            let omitted = observed["commands"].as_array().unwrap().len();
            for extension in ["stdout", "stderr"] {
                let key = format!("{omitted:03}.{extension}");
                assert_eq!(
                    expected.as_object_mut().unwrap().remove(&key),
                    Some(json!({"sha256":hash(b""),"bytes":0,"mode":0o600})),
                    "{name}: exact original name-removal capture omitted"
                );
            }
        }
        assert_eq!(
            actual, expected,
            "{name}: all retained attempt modes/bytes/complete frames"
        );
    }
    assert_eq!(
        comparison_tree(
            &root.join("native/config"),
            &root.join("native"),
            reference,
            true
        )
        .unwrap(),
        comparison_tree(
            &reference_root.join("config"),
            &reference_root,
            reference,
            false
        )
        .unwrap(),
        "{name}: complete selected configuration"
    );
    assert_eq!(
        semantic_tree(native.assets.clone()),
        semantic_tree(observed["assetFiles"].clone()),
        "{name}: complete immutable assets and selection"
    );
    assert_eq!(
        tree(&reference_root.join("config"), &reference_root).unwrap(),
        normalize(observed["configAfter"].clone(), &reference_root),
        "{name}: original rawconfig pins"
    );
}

const EXACT_ORACLE: &str = concat!(
    "release::attestation::deploy::oracle_tests::",
    "whole_deploy_matches_unchanged_python",
);

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
    exact && ignored && filter == Some(EXACT_ORACLE)
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
fn whole_oracle_mask_scope_requires_one_exact_ignored_test_process() {
    let args = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();
    assert!(exclusive_oracle(&args(&[
        EXACT_ORACLE,
        "--ignored",
        "--exact"
    ])));
    assert!(exclusive_oracle(&args(&[
        "--test-threads",
        "1",
        EXACT_ORACLE,
        "--nocapture",
        "--ignored",
        "--exact",
        "--format=pretty",
    ])));
    for values in [
        vec![EXACT_ORACLE, "--ignored"],
        vec!["--ignored", "--exact"],
        vec![EXACT_ORACLE, "--ignored", "--exact", "another_test"],
        vec!["short_filter", "--ignored", "--exact"],
    ] {
        assert!(!exclusive_oracle(&args(&values)));
    }
}

#[test]
#[ignore = "requires pinned controller Python; run explicitly by operations automation"]
fn whole_deploy_matches_unchanged_python() {
    // Only the documented dedicated exact ignored process can own a global
    // mask. Broad/parallel libtest selection refuses before any resource/mask.
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    assert!(
        exclusive_oracle(&arguments),
        "requires dedicated exact ignored oracle selection"
    );
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let prefix = std::env::var_os("PYTHON_CHECK_ENV")
        .map(PathBuf::from)
        .unwrap_or_else(|| repository.join("ops/ansible/.venv"));
    for scenario in SCENARIOS {
        eprintln!("whole host deploy case: {scenario}");
        let started = Instant::now();
        let fixture = Fixture::new();
        let mut owner = Owner::new(started, WHOLE, RESERVE).unwrap();
        let sources_before = source_pins(repository);
        // Match the real CLI's 077 creation scope for direct-core parity. This
        // is not the separate public stdout/runtime lifecycle execution proof.
        let mask = OracleMask::new();
        let previous_mask = mask.previous;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let completed = match &runtime {
            Ok(runtime) => runtime.block_on(async {
                let python = process::run(
                    &mut owner,
                    process::Invocation {
                        program: prefix.join("bin/python3"),
                        arguments: vec![
                            "-B".into(),
                            repository
                                .join("ops/ansible/tests/release_deploy_oracle.py")
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
                )
                .await;
                let reference = serde_json::from_slice::<Oracle>(&python.stdout);
                let native = if python.result.is_ok() {
                    match &reference {
                        Ok(reference) => {
                            native_case(reference, &fixture.0.join("native"), &mut owner, started)
                                .await
                        }
                        Err(_) => Err(io::Error::other("malformed whole original observation")),
                    }
                } else {
                    Err(io::Error::other("whole original child refused"))
                };
                (python, reference, native)
            }),
            Err(_) => {
                owner.record(Cause::Io);
                let _ = mask.restore();
                let _ = owner.finish();
                panic!("unable to create owned oracle runtime");
            }
        };
        drop(runtime);
        let (active_mask, restored_mask) = mask.restore();
        let finished = owner.finish();
        let sources_after = source_pins(repository);
        // All producer group/pipe, native file/lock and actual runtime owners
        // have completed before any verdict assertion or result unwrap.
        let (python, reference, native) = completed;
        assert_eq!(active_mask, nix::sys::stat::Mode::from_bits_truncate(0o077));
        assert_eq!(restored_mask, previous_mask);
        assert!(
            python.result.is_ok(),
            "{scenario}: bounded unchanged producer"
        );
        let proof = &python.observation;
        assert_eq!(proof.exit_status, Some(0));
        assert!(proof.reaped && proof.group_retired);
        assert!(proof.stdin_closed && proof.stdout_closed && proof.stderr_closed);
        assert!(proof.stdout_eof && proof.stderr_eof);
        assert!(proof.stdout_body_eof && proof.stderr_body_eof);
        assert!(proof.cleanup_errors.is_empty());
        assert!(python.stderr.is_empty());
        assert_eq!(proof.stdout_bytes, python.stdout.len() as u64);
        assert_eq!(proof.stdout_sha256, hash(&python.stdout));
        assert_eq!(
            sources_before.as_ref().unwrap(),
            sources_after.as_ref().unwrap()
        );
        let reference = reference.unwrap();
        assert_eq!(reference.scenario, *scenario);
        assert_eq!(
            reference.observation["originalBaselinePythonPins"],
            sources_before.unwrap()
        );
        compare(&reference, &native.unwrap(), &fixture.0, finished.is_ok());
    }
}
