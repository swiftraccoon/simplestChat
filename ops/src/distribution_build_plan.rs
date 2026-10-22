//! A nonexecuting recipe and measured selection of source bytes. Neither the
//! inventory nor this plan establishes complete Cargo I/O closure, source
//! revision, build success, authenticated provenance or runtime compatibility.

#[cfg(test)]
#[path = "distribution_build_fixture.rs"]
mod fixture;
#[path = "distribution_inventory.rs"]
mod inventory;
#[cfg(test)]
#[path = "distribution_build_plan_tests.rs"]
mod tests;

use super::manifest::{Selection, SourceIdentity, TARGET};
use serde::Serialize;
use std::path::Path;

pub use inventory::{SourceInventory, collect_inputs};

/// Private paths, directory names and operating-system errors are never output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Input,
    Changed,
    Bounds,
    Namespace,
    Close,
}
impl Failure {
    pub fn code(self) -> &'static str {
        match self {
            Self::Input => "ops_plan_input",
            Self::Changed => "ops_plan_changed",
            Self::Bounds => "ops_plan_bounds",
            Self::Namespace => "ops_plan_namespace",
            Self::Close => "ops_plan_close",
        }
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for Failure {}

/// Every executable/path slot remains unresolved. This is not a command string,
/// selected executable proof or permission to substitute ambient PATH programs.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Slot {
    CargoExecutable,
    RustcExecutable,
    RustLldExecutable,
    AuthenticatedHostToolsDirectory,
    StagingSourceRoot,
    CargoHome,
    TargetDirectory,
    TemporaryDirectory,
    HomeDirectory,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum Value {
    Literal(&'static str),
    Slot(Slot),
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentEntry {
    name: &'static str,
    value: Value,
}

/// Literal arguments and typed unresolved slots only; no shell is involved.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recipe {
    program: Slot,
    working_directory: Slot,
    arguments: Vec<Value>,
    environment_policy: &'static str,
    environment: Vec<EnvironmentEntry>,
    toolchain_version: &'static str,
    host_toolchain: &'static str,
}

/// Public output deliberately lacks success, executable digest, signature,
/// measured compiler version or readiness fields. Claims are caller-selected.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildPlan {
    schema_version: u8,
    kind: &'static str,
    claims: SourceIdentity,
    inventory: SourceInventory,
    recipe: Recipe,
    prerequisites: [&'static str; 9],
}

/// Construct a recipe without starting a process or resolving any slot. Cargo's
/// highest-priority encoded rustflags replace this workspace's global RPATH;
/// explicit `--target` keeps these target flags away from host proc macros.
/// <https://doc.rust-lang.org/cargo/reference/config.html#buildrustflags>
pub fn plan(selection: &Selection, inventory: &SourceInventory) -> BuildPlan {
    let literal = Value::Literal;
    let slot = Value::Slot;
    let environment = [
        ("CARGO_HOME", slot(Slot::CargoHome)),
        ("HOME", slot(Slot::HomeDirectory)),
        ("TMPDIR", slot(Slot::TemporaryDirectory)),
        ("PATH", slot(Slot::AuthenticatedHostToolsDirectory)),
        ("RUSTC", slot(Slot::RustcExecutable)),
        (
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER",
            slot(Slot::RustLldExecutable),
        ),
        (
            "CARGO_ENCODED_RUSTFLAGS",
            literal(concat!(
                "-C\u{1f}target-feature=+crt-static\u{1f}",
                "-C\u{1f}target-cpu=x86-64\u{1f}",
                "-C\u{1f}link-self-contained=yes\u{1f}",
                "-C\u{1f}linker-flavor=ld.lld",
            )),
        ),
        ("CARGO_INCREMENTAL", literal("0")),
        ("CARGO_TERM_COLOR", literal("never")),
        ("LC_ALL", literal("C")),
        ("TZ", literal("UTC")),
    ]
    .into_iter()
    .map(|(name, value)| EnvironmentEntry { name, value })
    .collect();
    BuildPlan {
        schema_version: 1,
        kind: "ops-build-plan",
        claims: selection.identity().clone(),
        inventory: inventory.clone(),
        recipe: Recipe {
            program: Slot::CargoExecutable,
            working_directory: Slot::StagingSourceRoot,
            arguments: vec![
                literal("build"),
                literal("--frozen"),
                literal("--release"),
                literal("-p"),
                literal("simplestchat-ops"),
                literal("--bin"),
                literal("simplestchat-ops"),
                literal("--target"),
                literal(TARGET),
                literal("--target-dir"),
                slot(Slot::TargetDirectory),
            ],
            environment_policy: "clearThenSet",
            environment,
            toolchain_version: "1.99.0",
            host_toolchain: "x86_64-unknown-linux-gnu",
        },
        prerequisites: [
            "freshOwnedStagingWithControlledAncestorsAndConfig",
            "inventoriedBytesStagedAndRebound",
            "authenticatedCargoRustcRustLldAndTargetComponents",
            "authenticatedHostToolsAndRuntimeClosure",
            "authenticatedPrepopulatedCargoRegistry",
            "isolatedCargoHomeWithoutUnmeasuredConfig",
            "ownedEmptyTargetDirectory",
            "auditedCompleteCargoInputClosure",
            "ownedBuildProcessAndClosedOutputEvidence",
        ],
    }
}

/// Convenience composition only. Selection validation belongs to `Selection`;
/// no source path, compiler or registry is inferred from its revision field.
pub fn inspect(selection: &Selection, source_root: &Path) -> Result<BuildPlan, Failure> {
    Ok(plan(selection, &collect_inputs(source_root)?))
}
