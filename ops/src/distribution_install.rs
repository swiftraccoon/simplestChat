//! Original-pinned first installation at one fixed managed path. This operation
//! never verifies a signature, runs a candidate, upgrades an existing binary or
//! switches production callers. Authentication remains the controller's job.
//!
//! The native CLI is synchronous and must own its calling thread: Linux signalfd
//! blocks INT/TERM on that thread and observes them at bounded I/O checkpoints.
//! Other application threads must not leave those process signals unblocked.
//! Mature synchronous ELF/platform helpers have checkpoints before and after;
//! blocked OS I/O, panic, SIGKILL and crash recovery have no cleanup guarantee.

#[path = "distribution_install_control.rs"]
mod control;
#[path = "distribution_install_files.rs"]
mod files;
#[path = "distribution_install_request.rs"]
mod request;
#[cfg(test)]
#[path = "distribution_install_tests.rs"]
mod tests;

use crate::{
    distribution::manifest::{ArtifactManifest, Selection, verify_pair},
    release::{assets::Workload, preflight},
};
use control::{Owner, Phase};
use files::{Access, Tree};
use request::Request;
use serde::Serialize;
use std::{fmt, path::Path};

/// Privacy-safe terminal status. Cleanup errors never replace an earlier known
/// body failure. A deadline is admission evidence, not a blocking-I/O wall bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Unproven,
    Busy,
    Deadline,
    Interrupted,
    Terminated,
}
impl Failure {
    /// Fixed public failure vocabulary, excluding paths and input content.
    pub fn code(self) -> &'static str {
        match self {
            Self::Busy => "ops_install_busy",
            _ => "ops_install_unproven",
        }
    }
    /// Preserve observed deadline/INT/TERM status after owned finalization.
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Deadline => 124,
            Self::Interrupted => 130,
            Self::Terminated => 143,
            Self::Unproven | Self::Busy => 1,
        }
    }
}
impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}
impl std::error::Error for Failure {}
type Result<T> = std::result::Result<T, Failure>;

/// Successful selected-original installation after namespace checks and checked
/// closes. Digest/count fields are measurements matched to the pinned request;
/// they do not assert that this library ran cryptographic authentication.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    schema_version: u8,
    passed: bool,
    changed: bool,
    manifest_sha256: String,
    manifest_bytes: u64,
    selection_sha256: String,
    selection_bytes: u64,
    binary_sha256: String,
    binary_bytes: u64,
}
impl Receipt {
    /// Whether this operation published a fresh selected executable. False
    /// means an exact already-installed artifact passed every check.
    pub fn changed(&self) -> bool {
        self.changed
    }
    fn new(request: &Request, changed: bool) -> Self {
        Self {
            schema_version: 1,
            passed: true,
            changed,
            manifest_sha256: request.manifest.sha256.clone(),
            manifest_bytes: request.manifest.bytes,
            selection_sha256: request.selection.sha256.clone(),
            selection_bytes: request.selection.bytes,
            binary_sha256: request.binary.sha256.clone(),
            binary_bytes: request.binary.bytes,
        }
    }
}

trait Host {
    fn platform(&self) -> Result<()>;
    fn lock(&self) -> Result<Workload>;
}
struct Native;
impl Host for Native {
    fn platform(&self) -> Result<()> {
        preflight::platform_only().map_err(|_| Failure::Unproven)
    }
    fn lock(&self) -> Result<Workload> {
        Workload::acquire().map_err(|error| {
            if error.busy() {
                Failure::Busy
            } else {
                Failure::Unproven
            }
        })
    }
}

/// Install a caller-authenticated, original-byte-pinned request at the fixed
/// root-owned helper destination. The request contains only schemaVersion plus
/// manifest/selection/binary {path,sha256,bytes}; all managed paths and modes are
/// compiled policy. Input paths are absolute and no root/owner bypass exists.
///
/// Fresh publication uses no-replace rename. Exact already-installed bytes and
/// mode are a filesystem no-op. Different bytes/modes, symlinks, special files,
/// unfinished journals and foreign pending files refuse. Errors after a known
/// publication retain the visible file; retries must prove the same identity.
/// The existing workload flock remains held through file checks and closes.
///
/// # Errors
/// Returns a fixed failure for invalid pins, unsupported host, contention,
/// deadline/cancellation, changed namespace, I/O or finalization uncertainty.
/// The single-thread cooperative signal scope is documented at module level.
pub fn install(path: &Path, original_sha256: &str) -> Result<Receipt> {
    let owner = Owner::native()?;
    run(path, original_sha256, Access::default(), &Native, owner)
}

fn run(
    path: &Path,
    selected: &str,
    access: Access,
    host: &impl Host,
    mut owner: Owner,
) -> Result<Receipt> {
    let mut tree = None;
    let mut lock = None;
    let result = (|| {
        owner.check(Phase::Body)?;
        request::path(path)?;
        if !request::digest(selected) {
            return Err(Failure::Unproven);
        }
        tree = Some(Tree::open(access)?);
        let tree = tree.as_mut().unwrap();
        let (_, original) = tree.request(path, selected, &mut owner)?;
        let request = Request::decode(&original)?;
        host.platform()?;
        owner.check(Phase::Body)?;
        let manifest = tree.input(request.manifest.clone(), 0o600, &mut owner)?;
        let selection = tree.input(request.selection.clone(), 0o600, &mut owner)?;
        let binary = tree.input(request.binary.clone(), 0o755, &mut owner)?;
        let original_manifest = tree.read(manifest, &mut owner, Phase::Body, true)?;
        let original_selection = tree.read(selection, &mut owner, Phase::Body, true)?;
        tree.read(binary, &mut owner, Phase::Body, false)?;
        let decoded =
            ArtifactManifest::decode(&original_manifest).map_err(|_| Failure::Unproven)?;
        let selected = Selection::decode(&original_selection).map_err(|_| Failure::Unproven)?;
        owner.check(Phase::Body)?;
        let pair = verify_pair(&decoded, &tree.physical(binary), &selected)
            .map_err(|_| Failure::Unproven)?;
        owner.check(Phase::Body)?;
        if pair.executable().sha256 != request.binary.sha256
            || pair.executable().bytes != request.binary.bytes
        {
            return Err(Failure::Unproven);
        }
        tree.read(binary, &mut owner, Phase::Body, false)?;
        tree.access.event("pair-verified", "")?;
        owner.check(Phase::Body)?;
        let destination = tree.destination()?;
        tree.verify_directories()?;
        owner.check(Phase::Body)?;
        lock = Some(host.lock()?);
        lock.as_ref()
            .unwrap()
            .validate()
            .map_err(|_| Failure::Unproven)?;
        let changed = !tree.existing(destination, &request.binary, &mut owner)?;
        if changed {
            lock.as_ref()
                .unwrap()
                .validate()
                .map_err(|_| Failure::Unproven)?;
            tree.publish(destination, binary, &mut owner, lock.as_ref().unwrap())?;
        }
        owner.check(Phase::Body)?;
        Ok(Receipt::new(&request, changed))
    })();
    let files = tree
        .take()
        .map(|tree| tree.finish(&mut owner))
        .unwrap_or(Ok(()));
    // Both finalizers run even after a body/peer failure. The flock is released
    // only after every retained installer File has received its close attempt.
    let guard = lock
        .take()
        .map(Workload::finish)
        .transpose()
        .map(|_| ())
        .map_err(|_| Failure::Unproven);
    let ended = owner.finish();
    let receipt = result?;
    files?;
    guard?;
    ended?;
    Ok(receipt)
}
