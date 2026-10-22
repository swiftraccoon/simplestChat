//! Local executable identity and bounded ELF linkage facts. This module never
//! runs a candidate or proves its source, libc provenance or host compatibility.

#[path = "distribution_build_plan.rs"]
pub mod build_plan;
#[path = "distribution_elf.rs"]
mod elf;
#[cfg(test)]
#[path = "distribution_fixture.rs"]
pub(crate) mod fixture;
#[path = "distribution_manifest.rs"]
pub mod manifest;
#[cfg(test)]
#[path = "distribution_tests.rs"]
mod tests;

use crate::files::Snapshot;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path};

/// The existing scanner executable ceiling; metadata has separate small caps.
pub const MAX_BYTES: u64 = 256 * 1024 * 1024;

/// Only the already supported managed-host architecture is admitted. An ELF
/// machine identifier does not establish Linux/kernel/libc compatibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Architecture {
    #[serde(rename = "x86_64")]
    X86_64,
}

/// File type as encoded in the ELF header. `Dynamic` alone does not prove PIE.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FileType {
    Executable,
    Dynamic,
}

/// Fixed diagnostic facts only: no symbol names, loader paths, dependency names,
/// notes, section contents or arbitrary strings enter the public projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutableIdentity {
    pub sha256: String,
    pub bytes: u64,
    pub architecture: Architecture,
    pub elf_class: u8,
    pub little_endian: bool,
    pub file_type: FileType,
    pub program_headers: u16,
    pub no_interpreter: bool,
    pub needed_count: u16,
    pub no_needed: bool,
    pub pie_flag: bool,
    pub bind_now: bool,
    pub non_executable_stack: bool,
    pub no_writable_executable_load: bool,
    pub relro_present: bool,
    pub no_rpath: bool,
    pub no_runpath: bool,
    pub no_text_relocations: bool,
    pub no_loader_hooks: bool,
}

/// Caller-selected identity, not an authenticated claim. The caller still owns
/// signature verification and the source of these expected values.
#[derive(Clone, Debug)]
pub struct SelectedExecutable {
    sha256: String,
    bytes: u64,
    architecture: Architecture,
}
impl SelectedExecutable {
    pub fn new(sha256: &str, bytes: u64, architecture: Architecture) -> Result<Self, Failure> {
        if sha256.len() != 64
            || !sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || !(1..=MAX_BYTES).contains(&bytes)
        {
            return Err(Failure::Selection);
        }
        Ok(Self {
            sha256: sha256.into(),
            bytes,
            architecture,
        })
    }
}

/// Private inputs and operating-system/parser error messages are never formatted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Input,
    Changed,
    Read,
    Header,
    ProgramHeaders,
    Segment,
    Dynamic,
    Strings,
    Selection,
    Identity,
}
impl Failure {
    pub fn code(self) -> &'static str {
        match self {
            Self::Input => "executable_input_refused",
            Self::Changed => "executable_input_changed",
            Self::Read => "executable_unreadable",
            Self::Header => "executable_elf_header",
            Self::ProgramHeaders => "executable_program_headers",
            Self::Segment => "executable_segment",
            Self::Dynamic => "executable_dynamic_table",
            Self::Strings => "executable_dynamic_strings",
            Self::Selection => "executable_selection",
            Self::Identity => "executable_identity_mismatch",
        }
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.code())
    }
}
impl std::error::Error for Failure {}
impl From<crate::Error> for Failure {
    fn from(error: crate::Error) -> Self {
        match error {
            crate::Error::Changed => Self::Changed,
            _ => Self::Input,
        }
    }
}

/// Inspect a nonempty regular, nonlink file without executing it. Whole-file SHA
/// includes overlays/trailing bytes. Every parser read/seek has snapshot guards;
/// final namespace checks and explicit successful closes precede the result.
/// This supports bounded ELF64 little-endian x86-64 ET_EXEC/ET_DYN with an entry
/// point in an executable load segment. The Linux 4 KiB base-page domain refuses
/// overlapping rounded LOAD spans, requires a complete original program-table
/// mapping and checks any PT_PHDR against it. Extended counts are refused.
pub fn inspect(path: &Path) -> Result<ExecutableIdentity, Failure> {
    inspect_after(path, |_| {})
}

fn inspect_after(path: &Path, mut after: impl FnMut(&str)) -> Result<ExecutableIdentity, Failure> {
    let mut snapshot = Snapshot::open(path, MAX_BYTES)?;
    let result = inspect_snapshot(&mut snapshot, &mut after);
    let closed = snapshot.finish().map_err(Failure::from);
    let identity = result?;
    closed?;
    Ok(identity)
}

fn inspect_snapshot(
    snapshot: &mut Snapshot,
    after: &mut impl FnMut(&str),
) -> Result<ExecutableIdentity, Failure> {
    let mut digest = Sha256::new();
    let mut count = 0_u64;
    let mut buffer = [0; 65536];
    let mut stream = snapshot.checked_stream();
    loop {
        let size = stream.read(&mut buffer).map_err(|_| Failure::Read)?;
        if size == 0 {
            break;
        }
        count = count.checked_add(size as u64).ok_or(Failure::Read)?;
        if count > MAX_BYTES {
            return Err(Failure::Read);
        }
        digest.update(&buffer[..size]);
    }
    if count != snapshot.length() {
        return Err(Failure::Changed);
    }
    let sha256 = digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    after("hashed");
    let identity = elf::inspect(snapshot, sha256)?;
    after("parsed");
    Ok(identity)
}

/// Match independently selected original bytes and architecture. Success proves
/// this snapshot matches those values; it does not authenticate their origin.
pub fn verify(path: &Path, selected: &SelectedExecutable) -> Result<ExecutableIdentity, Failure> {
    let identity = inspect(path)?;
    if identity.sha256 != selected.sha256
        || identity.bytes != selected.bytes
        || identity.architecture != selected.architecture
    {
        return Err(Failure::Identity);
    }
    Ok(identity)
}
