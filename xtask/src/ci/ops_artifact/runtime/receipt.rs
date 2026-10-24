//! A closed conditional keeper observation. Expected pins come only from the
//! original authenticated capability and controller-created selection/request.

use super::super::acquire::PairPins;
use super::{failure, request::Request};
use crate::tooling::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, value::RawValue};
use simplestchat_ops::distribution::ExecutableIdentity;
use std::{ffi::OsString, path::Path};

pub(super) const CASES: [(&str, u8); 28] = [
    ("source-help", 0),
    ("source-invalid-cli", 2),
    ("source-forbidden-root", 2),
    ("source-inspect", 0),
    ("source-pair", 0),
    ("install-foreign-bytes", 1),
    ("install-foreign-mode", 1),
    ("install-first", 0),
    ("install-repeat", 0),
    ("installed-help", 0),
    ("installed-inspect", 0),
    ("installed-pair", 0),
    ("preflight-ordinary", 0),
    ("preflight-base", 0),
    ("preflight-fetch", 0),
    ("preflight-force", 0),
    ("preflight-invalid-shape", 1),
    ("preflight-unknown-helper", 1),
    ("preflight-invalid-digest", 1),
    ("preflight-combined", 1),
    ("preflight-cli", 2),
    ("preflight-unprivileged", 1),
    ("preflight-mode", 1),
    ("preflight-hash", 1),
    ("release-manifest", 0),
    ("release-manifest-private", 1),
    ("installed-final-inspect", 0),
    ("installed-final-pair", 0),
];

pub(super) struct Pins {
    pub run_id: String,
    pub selection: Value,
    pub selection_json: String,
    pub artifact_id: u64,
    pub pair: PairPins,
    pub transfer_sha256: String,
    pub transfer_bytes: u64,
    pub install_request_json: String,
    pub install_request_sha256: String,
    pub executable_json: String,
    pub cloud_sha512: String,
}
impl Pins {
    pub fn new(
        request: &Request,
        pair: PairPins,
        executable: &ExecutableIdentity,
        run_id: String,
        transfer_sha256: String,
        transfer_bytes: u64,
        cloud_sha512: String,
    ) -> Result<Self> {
        if run_id.len() != 32
            || !run_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !super::super::digest(&transfer_sha256)
            || transfer_bytes == 0
            || transfer_bytes > super::TRANSFER
            || pair.selection().sha256() != super::super::sha(&request.selected_bytes)
            || pair.selection().bytes() != request.selected_bytes.len() as u64
        {
            return Err(failure(1));
        }
        let guest = "/root/simplestchat-vm-fixture/ops-input";
        let install = serde_json::json!({
            "schemaVersion":1,
            "manifest":{
                "path":format!("{guest}/ops-release.json"),
                "sha256":pair.manifest().sha256(),"bytes":pair.manifest().bytes()
            },
            "selection":{
                "path":format!("{guest}/ops-selection.json"),
                "sha256":pair.selection().sha256(),"bytes":pair.selection().bytes()
            },
            "binary":{
                "path":format!("{guest}/simplestchat-ops"),
                "sha256":pair.binary().sha256(),"bytes":pair.binary().bytes()
            }
        });
        let install_request_json = serde_json::to_string(&install).map_err(|_| failure(1))?;
        let executable_json = serde_json::to_string(executable).map_err(|_| failure(1))?;
        if install_request_json.len() > 16384 || executable_json.len() > 4096 {
            return Err(failure(1));
        }
        let selected = serde_json::to_value(executable).map_err(|_| failure(1))?;
        if selected["sha256"] != pair.binary().sha256()
            || selected["bytes"] != pair.binary().bytes()
        {
            return Err(failure(1));
        }
        Ok(Self {
            run_id,
            selection: serde_json::to_value(request.selection.identity())
                .map_err(|_| failure(1))?,
            selection_json: String::from_utf8(request.selected_bytes.clone())
                .map_err(|_| failure(1))?,
            artifact_id: request.artifact_id,
            pair,
            transfer_sha256,
            transfer_bytes,
            install_request_sha256: super::super::sha(install_request_json.as_bytes()),
            install_request_json,
            executable_json,
            cloud_sha512,
        })
    }
    pub fn arguments(&self, source: &Path, output: &Path) -> Vec<OsString> {
        let mut args: Vec<OsString> = ["-I", "-B", "-X"].into_iter().map(OsString::from).collect();
        args.push(format!("pycache_prefix={}", output.join("python-cache").display()).into());
        args.push(source.join("build/security_vm_ops.py").into_os_string());
        for (flag, value) in [
            ("--output", output.join("keeper").into_os_string()),
            ("--run-id", self.run_id.clone().into()),
            ("--selection-json", self.selection_json.clone().into()),
            ("--artifact-id", self.artifact_id.to_string().into()),
            ("--transfer-sha256", self.transfer_sha256.clone().into()),
            ("--transfer-bytes", self.transfer_bytes.to_string().into()),
            ("--manifest-sha256", self.pair.manifest().sha256().into()),
            (
                "--manifest-bytes",
                self.pair.manifest().bytes().to_string().into(),
            ),
            ("--selection-sha256", self.pair.selection().sha256().into()),
            (
                "--selection-bytes",
                self.pair.selection().bytes().to_string().into(),
            ),
            ("--binary-sha256", self.pair.binary().sha256().into()),
            (
                "--binary-bytes",
                self.pair.binary().bytes().to_string().into(),
            ),
            (
                "--install-request-json",
                self.install_request_json.clone().into(),
            ),
            (
                "--install-request-sha256",
                self.install_request_sha256.clone().into(),
            ),
            ("--executable-json", self.executable_json.clone().into()),
        ] {
            args.push(flag.into());
            args.push(value);
        }
        args
    }
}

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum Nullable<T> {
    Value(T),
    Null(()),
}
impl<T> Nullable<T> {
    fn as_ref(&self) -> Option<&T> {
        match self {
            Self::Value(value) => Some(value),
            Self::Null(()) => None,
        }
    }
    fn into_option(self) -> Option<T> {
        match self {
            Self::Value(value) => Some(value),
            Self::Null(()) => None,
        }
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Install {
    first_changed: Nullable<bool>,
    second_changed: Nullable<bool>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Row {
    pub name: String,
    pub exit_status: u8,
    pub passed: bool,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Lifecycle {
    qemu_reaped: bool,
    qemu_return_code: Nullable<i16>,
    authenticated_ssh: bool,
    drainer_completed: bool,
    qmp_connected: bool,
    qmp_greeting_received: bool,
    qmp_capabilities_negotiated: bool,
    qmp_eof_observed: bool,
    qmp_passed: bool,
    cloud_init_passed: bool,
    close_returned: bool,
}
impl Lifecycle {
    fn positive(&self) -> bool {
        self.qemu_reaped
            && self
                .qemu_return_code
                .as_ref()
                .is_some_and(|code| (-255..=255).contains(code))
            && self.authenticated_ssh
            && self.drainer_completed
            && self.qmp_connected
            && self.qmp_greeting_received
            && self.qmp_capabilities_negotiated
            && self.qmp_eof_observed
            && self.qmp_passed
            && self.cloud_init_passed
            && self.close_returned
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Wire {
    schema_version: u8,
    scope: String,
    run_id: String,
    selection: Box<RawValue>,
    artifact_id: u64,
    transfer_sha256: String,
    transfer_bytes: u64,
    manifest_sha256: String,
    manifest_bytes: u64,
    selection_sha256: String,
    selection_bytes: u64,
    binary_sha256: String,
    binary_bytes: u64,
    install_request_sha256: String,
    cloud_image_sha512: String,
    install: Install,
    cases: Vec<Row>,
    guest_executed: bool,
    runtime_validated: bool,
    install_validated: bool,
    cleanup_passed: bool,
    passed: bool,
    failure: Nullable<String>,
    vm_lifecycle: Nullable<Lifecycle>,
}
pub(super) struct Observation {
    pub install: Install,
    pub cases: Vec<Row>,
    pub lifecycle: Lifecycle,
}
fn matching_identity(wire: &Wire, pins: &Pins) -> Result<bool> {
    let selected = simplestchat_ops::distribution::manifest::Selection::decode(
        wire.selection.get().as_bytes(),
    )
    .map_err(|_| failure(1))?;
    let selected = serde_json::to_value(selected.identity()).map_err(|_| failure(1))?;
    Ok(wire.schema_version == 1
        && wire.scope == "OpsDebianKeeperReceipt"
        && wire.run_id == pins.run_id
        && selected == pins.selection
        && wire.artifact_id == pins.artifact_id
        && wire.transfer_sha256 == pins.transfer_sha256
        && wire.transfer_bytes == pins.transfer_bytes
        && wire.manifest_sha256 == pins.pair.manifest().sha256()
        && wire.manifest_bytes == pins.pair.manifest().bytes()
        && wire.selection_sha256 == pins.pair.selection().sha256()
        && wire.selection_bytes == pins.pair.selection().bytes()
        && wire.binary_sha256 == pins.pair.binary().sha256()
        && wire.binary_bytes == pins.pair.binary().bytes()
        && wire.install_request_sha256 == pins.install_request_sha256
        && wire.cloud_image_sha512 == pins.cloud_sha512)
}

/// Negative diagnostics are fixed labels, never a successful receipt or raw text.
pub(super) fn failure_message(bytes: &[u8], pins: &Pins) -> Option<&'static str> {
    if bytes.is_empty() || bytes.len() as u64 > super::SMALL {
        return None;
    }
    let wire: Wire = serde_json::from_slice(bytes).ok()?;
    if wire.passed || !matching_identity(&wire, pins).ok()? {
        return None;
    }
    match wire.failure.as_ref()?.as_str() {
        "guard" => Some("Ops Debian runtime keeper guard failed."),
        "boot" => Some("Ops Debian runtime keeper boot failed."),
        "stage" => Some("Ops Debian runtime keeper staging failed."),
        "cases" => Some("Ops Debian runtime keeper cases failed."),
        "cleanup" => Some("Ops Debian runtime keeper cleanup failed."),
        "deadline" => Some("Ops Debian runtime keeper reported its deadline."),
        "cancelled" => Some("Ops Debian runtime keeper reported cancellation."),
        _ => None,
    }
}
pub(super) fn admit(bytes: &[u8], pins: &Pins) -> Result<Observation> {
    if bytes.is_empty() || bytes.len() as u64 > super::SMALL {
        return Err(failure(1));
    }
    let wire: Wire = serde_json::from_slice(bytes).map_err(|_| failure(1))?;
    if !matching_identity(&wire, pins)?
        || wire.cases.len() != CASES.len()
        || wire.cases.iter().zip(CASES).any(|(row, (name, status))| {
            row.name != name || row.exit_status != status || !row.passed
        })
        || wire.install.first_changed.as_ref() != Some(&true)
        || wire.install.second_changed.as_ref() != Some(&false)
        || !wire.guest_executed
        || !wire.runtime_validated
        || !wire.install_validated
        || !wire.cleanup_passed
        || !wire.passed
        || wire.failure.as_ref().is_some()
        || wire.vm_lifecycle.as_ref().is_none_or(|l| !l.positive())
    {
        return Err(failure(1));
    }
    Ok(Observation {
        install: wire.install,
        cases: wire.cases,
        lifecycle: wire.vm_lifecycle.into_option().ok_or_else(|| failure(1))?,
    })
}
pub(super) fn summary(pins: &Pins, observed: &Observation) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "schemaVersion":1,"scope":"AuthenticatedOpsDebianRuntimeAndFirstInstall",
        "runId":pins.run_id,"selection":pins.selection,"artifactId":pins.artifact_id,
        "manifestSha256":pins.pair.manifest().sha256(),"manifestBytes":pins.pair.manifest().bytes(),
        "selectionSha256":pins.pair.selection().sha256(),
        "selectionBytes":pins.pair.selection().bytes(),
        "binarySha256":pins.pair.binary().sha256(),"binaryBytes":pins.pair.binary().bytes(),
        "cloudImageSha512":pins.cloud_sha512,"install":observed.install,"cases":observed.cases,
        "guestExecuted":true,"runtimeValidated":true,"installValidated":true,"cleanupPassed":true,
        "passed":true,"failure":null,"vmLifecycle":observed.lifecycle
    }))
    .map_err(|_| failure(1))?;
    if bytes.len() as u64 > super::SMALL {
        return Err(failure(1));
    }
    Ok(bytes)
}
