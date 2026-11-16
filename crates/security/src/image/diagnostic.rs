//! The public diagnostic of one scanner run: fixed classes of how it ended,
//! and the size and digest of the private log of its engine command, never
//! anything the scanner or the engine said.

use crate::{
    Fault, Result, digest,
    document::Document,
    image::{policy, sandbox::Limits},
    require,
};
use rustix::fs::{Mode, OFlags};
use std::{fs::File, io::Read, path::Path};

/// A log that is no regular file within its bound, or one that changed
/// while it was read, or an outcome of its command that is no record.
const LOG: &str = "image_scanner_diagnostic_log";
/// The most bytes of the outcome the log's command left.
const OUTCOME: u64 = 65_536;
/// The exit status of a scanner that reports what it found.
pub const FINDINGS: u64 = 10;

/// One scanner run, as its diagnostic is told of it.
#[derive(Clone, Copy, Debug)]
pub struct Scanned<'a> {
    /// The scanner.
    pub tool: &'a str,
    /// Its exit status, when the engine's command came to an end.
    pub status: Option<i32>,
    /// What the engine said of the container once its identity was proved.
    pub owned: Option<&'a Document>,
    /// Whether the container was removed and is proved gone.
    pub cleaned: bool,
    /// The bytes of scratch space it had.
    pub scratch: u64,
    /// The private log of the engine's command, when one was kept.
    pub log: Option<&'a Path>,
}

/// Whether `value` is a whole number, however large.
fn integer(value: &Document) -> bool {
    matches!(value, Document::Number(number) if number.is_i64() || number.is_u64())
}

/// The bytes of the log at `path`: a regular file of at most `limit`
/// bytes that is not reached through a link and does not change while it
/// is read.
fn kept(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let file = rustix::fs::open(path, flags, Mode::empty()).map_err(|_| Fault(LOG))?;
    let file = File::from(file);
    let metadata = file.metadata().map_err(|_| Fault(LOG))?;
    require(metadata.is_file() && metadata.len() <= limit, LOG)?;
    let mut content = Vec::new();
    let read = file.take(limit.saturating_add(1)).read_to_end(&mut content);
    read.map_err(|_| Fault(LOG))?;
    require(content.len() as u64 == metadata.len(), LOG)?;
    Ok(content)
}

/// The diagnostic of the run `scanned`, held to `limits`. A run is
/// classified by the first of: its command timed out, said more than its
/// bound, the container ran out of memory, the log tells of a full disk,
/// the command failed, the container's state is no exited one that agrees
/// with the scanner's status, its removal is not proved, the scanner passed
/// or reported findings, and otherwise it failed.
pub fn diagnostic(scanned: &Scanned<'_>, limits: &Limits) -> Result<Document> {
    let (log, outcome) = match scanned.log {
        Some(path) if path.exists() => {
            let log = kept(path, limits.log)?;
            let outcome = policy::report(&path.with_extension("outcome.json"), OUTCOME)?;
            require(outcome.fields().is_some(), LOG)?;
            (Some(log), outcome)
        }
        _ => (None, Document::Record(Vec::new())),
    };
    let exit = match scanned.status {
        Some(status) => Some(Document::Number(i64::from(status).into())),
        None => outcome
            .get("exitStatus")
            .filter(|value| integer(value))
            .cloned(),
    };
    let field = |name: &str| scanned.owned.and_then(|owned| owned.get(name));
    let container = field("exitStatus").filter(|value| integer(value));
    let oom = match field("oomKilled") {
        Some(Document::Bool(oom)) => Some(*oom),
        _ => None,
    };
    let zero = Document::Number(0_u64.into());
    let byte = container
        .and_then(Document::whole)
        .is_some_and(|code| (0..=255).contains(&code));
    let agrees = field("state").and_then(Document::text) == Some("exited")
        && container == exit.as_ref()
        && !(oom == Some(true) && exit.as_ref() == Some(&zero));
    let valid = byte && oom.is_some() && (scanned.status.is_none() || agrees);
    let timed = outcome.get("timedOut") == Some(&Document::Bool(true));
    let limited = outcome.get("outputLimited") == Some(&Document::Bool(true));
    let full = log.as_ref().is_some_and(|log| {
        let lower = log.to_ascii_lowercase();
        let needle = b"no space left on device";
        lower.windows(needle.len()).any(|window| window == needle)
    });
    let findings = Document::Number(FINDINGS.into());
    let classification = if timed {
        "timed_out"
    } else if limited {
        "output_limit"
    } else if oom == Some(true) {
        "out_of_memory"
    } else if full {
        "storage_exhausted"
    } else if scanned.status.is_none() {
        "controller_failed"
    } else if !valid {
        "container_state_invalid"
    } else if !scanned.cleaned {
        "cleanup_unverified"
    } else if exit.as_ref() == Some(&zero) {
        "completed"
    } else if ["grype", "gitleaks"].contains(&scanned.tool) && exit.as_ref() == Some(&findings) {
        "findings"
    } else {
        "scanner_failed"
    };
    let or_null = |value: Option<Document>| value.unwrap_or(Document::Null);
    Ok(Document::record([
        ("schemaVersion", 1_u64.into()),
        ("tool", scanned.tool.into()),
        ("classification", classification.into()),
        ("exitStatus", or_null(exit)),
        ("containerExitStatus", or_null(container.cloned())),
        ("oomKilled", or_null(oom.map(Document::Bool))),
        ("containerStateValid", valid.into()),
        ("cleanupVerified", scanned.cleaned.into()),
        ("timedOut", timed.into()),
        ("outputLimited", limited.into()),
        ("storageExhausted", full.into()),
        (
            "logBytes",
            or_null(log.as_ref().map(|log| log.len().into())),
        ),
        (
            "logSha256",
            or_null(log.as_deref().map(|log| digest(log).into())),
        ),
        (
            "limits",
            Document::record([
                ("temporaryBytes", scanned.scratch.into()),
                ("memoryBytes", (2_u64 << 30).into()),
                ("swapAdditionalBytes", 0_u64.into()),
                ("cpus", 2_u64.into()),
                ("pids", 128_u64.into()),
                ("seconds", limits.seconds.into()),
                ("logBytes", limits.log.into()),
            ]),
        ),
    ]))
}

#[cfg(test)]
#[path = "diagnostic_tests.rs"]
mod tests;
