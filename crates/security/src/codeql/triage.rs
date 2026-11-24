//! `simplestchat-security codeql triage health|sarif|plan|check|apply`:
//! exact CodeQL reviews enforced on a SARIF report or on GitHub's stored
//! alerts, and authorized dismissals planned and applied apart. A SARIF
//! check needs no token; health, plan and check only read; apply needs an
//! explicit flag, an unchanged plan and a fresh exact read of each alert.
//! No query, path, severity class or remote dismissal is an exclusion.
//!
//! Where the reference's interpreter raised, the step stops with the name
//! of the class it raised, as the reference's evidence names it: a fixed
//! code is in lowercase words, and a raised class begins with a capital.

#[path = "triage_command.rs"]
pub mod command;
#[path = "triage_findings.rs"]
pub mod findings;
#[path = "triage_github.rs"]
pub mod github;
#[path = "triage_plan.rs"]
pub mod plan;
#[path = "triage_sarif.rs"]
pub mod sarif;

pub use command::run;

use super::{Stop, unmarked};
use crate::{
    Fault, Result,
    document::{Document, Unread},
    require,
};
use rustix::{
    fs::{Mode, OFlags},
    io::Errno,
};
use std::{
    fs::File,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

/// The most bytes of a report or plan that is read.
pub const MAX_REPORT: u64 = 64 * 1024 * 1024;
/// The most bytes of one source, or of one answer of GitHub's.
pub const MAX_SOURCE: u64 = 4 * 1024 * 1024;
/// The most entries of one page of GitHub's answers.
pub const MAX_PAGE: usize = 100;
/// The most pages that are asked for.
pub const MAX_PAGES: usize = 20;
/// The most alerts that one application dismisses.
pub const MAX_APPLY: usize = 128;
/// How long a reading operation may take, in seconds.
pub const MAX_SECONDS: f64 = 300.0;
/// How long an operation that writes may take, in seconds.
pub const MAX_APPLY_SECONDS: f64 = 900.0;
/// The least time between two writes, in seconds.
pub const WRITE_INTERVAL: f64 = 1.0;
/// The most findings of one report.
pub const MAX_FINDINGS: usize = 2000;
/// The least score of a high finding.
pub const HIGH_SCORE: f64 = 7.0;
/// The least score of a critical finding.
pub const CRITICAL_SCORE: f64 = 9.0;
/// The greatest score there is.
pub const MAX_SCORE: f64 = 10.0;
/// The least score of a medium finding.
pub const MEDIUM_SCORE: f64 = 4.0;
/// The most bytes of the policy or of a review that is read.
pub const MAX_POLICY: u64 = 1024 * 1024;
/// The languages a report may hold runs of.
pub const LANGUAGES: [&str; 5] = [
    "actions",
    "javascript-typescript",
    "python",
    "rust",
    "c-cpp",
];
/// The severities a finding may have.
pub const SEVERITIES: [&str; 5] = ["critical", "high", "medium", "low", "advisory"];
/// The fields of a location's range, in the order they are bound.
pub const REGION: [&str; 4] = ["start_line", "end_line", "start_column", "end_column"];
/// The explicit false-positive decisions, below the checkout.
pub const REVIEWS: [&str; 2] = [
    "security/codeql-review-2026-09-30.json",
    "security/codeql-native-review-2026-09-30.json",
];
/// The reviewed exceptions, below the checkout.
pub const POLICY: &str = "security/exceptions.json";

/// What the reference's interpreter raised for a value of the wrong kind.
pub(crate) const VALUE: Fault = Fault("ValueError");
/// What it raised for a field that is not there.
pub(crate) const KEY: Fault = Fault("KeyError");
/// What it raised for a list or a record asked to be a member of a set.
pub(crate) const UNHASHABLE: Fault = Fault("TypeError");
/// What it raised for a command past its bounds.
pub(crate) const PROCESS: Fault = Fault("ProcessError");
/// The empty list a missing field stands for.
pub(crate) static NO_ITEMS: Document = Document::List(Vec::new());
/// The empty record a missing field stands for.
pub(crate) static NO_FIELDS: Document = Document::Record(Vec::new());
/// The mode of every file of evidence, within the caller's mask.
const PRIVATE: Mode = Mode::from_raw_mode(0o600);

/// The files a plan is bound to: the policy, and the two reviews in the
/// order they are read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ledger {
    /// The reviewed exceptions.
    pub policy: PathBuf,
    /// The explicit false-positive decisions.
    pub reviews: [PathBuf; 2],
}

impl Ledger {
    /// The ledger of the checkout `root`.
    pub fn of(root: &Path) -> Self {
        Self {
            policy: root.join(POLICY),
            reviews: REVIEWS.map(|name| root.join(name)),
        }
    }
}

/// The class of what the reference's interpreter raised for a failed
/// system call of `errno`, as it chose a class by the error's number.
pub(crate) fn raised(errno: Errno) -> Fault {
    Fault(match errno {
        Errno::AGAIN | Errno::ALREADY | Errno::INPROGRESS => "BlockingIOError",
        Errno::CHILD => "ChildProcessError",
        Errno::PIPE | Errno::SHUTDOWN => "BrokenPipeError",
        Errno::CONNABORTED => "ConnectionAbortedError",
        Errno::CONNREFUSED => "ConnectionRefusedError",
        Errno::CONNRESET => "ConnectionResetError",
        Errno::EXIST => "FileExistsError",
        Errno::NOENT => "FileNotFoundError",
        Errno::ISDIR => "IsADirectoryError",
        Errno::NOTDIR => "NotADirectoryError",
        Errno::INTR => "InterruptedError",
        Errno::ACCESS | Errno::PERM => "PermissionError",
        Errno::SRCH => "ProcessLookupError",
        Errno::TIMEDOUT => "TimeoutError",
        _ => "OSError",
    })
}

/// The class of what the reference's interpreter raised for `error`.
pub(crate) fn raised_by(error: &std::io::Error) -> Fault {
    error.raw_os_error().map_or(Fault("OSError"), |number| {
        raised(Errno::from_raw_os_error(number))
    })
}

/// The refusal here of a stop of the steps that read sources: the class
/// the reference's interpreter raised where the step stood in for it, and
/// otherwise the step's own code. What the vendor's verifier complains of
/// was the reference's `IntegrityError`.
pub(crate) fn stopped(stop: Stop) -> Fault {
    match stop {
        Stop::Code(code) => classified(Fault(code)),
        Stop::Problem(_) => Fault("IntegrityError"),
    }
}

/// The refusal here of a code of the shared steps: the class the
/// reference's interpreter raised where the shared step stood in for it,
/// and otherwise the code itself.
pub(crate) fn classified(fault: Fault) -> Fault {
    match fault.0 {
        "command_stdout_limit_exceeded"
        | "command_stderr_limit_exceeded"
        | "command_timed_out"
        | "command_lease_lost"
        | "scanner_command_unobserved" => PROCESS,
        "tool_entry_missing" => Fault("FileNotFoundError"),
        "tool_entry_unavailable" => Fault("OSError"),
        "codeql_rust_source_text" | "codeql_wrap_unread" => Fault("UnicodeDecodeError"),
        "codeql_wrap_directory_missing" => KEY,
        _ => fault,
    }
}

/// The whole of the regular, nonempty file `path` of at most `limit`
/// bytes, read through no final link, as the reference's bounded reader
/// read one: a directory is refused as its interpreter refused to open
/// one, and anything else that is no regular file by its code.
pub(crate) fn bounded_file(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let file = rustix::fs::open(path, flags, Mode::empty()).map_err(raised)?;
    let metadata = rustix::fs::fstat(&file).map_err(raised)?;
    let kind = rustix::fs::FileType::from_raw_mode(metadata.st_mode);
    require(kind != rustix::fs::FileType::Directory, "IsADirectoryError")?;
    require(
        kind == rustix::fs::FileType::RegularFile,
        "nonregular_tool_file",
    )?;
    let size = u64::try_from(metadata.st_size).unwrap_or(u64::MAX);
    require(size > 0 && size <= limit, "tool_file_size")?;
    let mut content = Vec::new();
    File::from(file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut content)
        .map_err(|error| raised_by(&error))?;
    require(content.len() as u64 == size, "tool_file_changed")?;
    Ok(content)
}

/// Create `path` and nothing else with `content`, readable by the caller
/// alone within its mask, and hand its bytes to the disk before it is
/// published.
pub(crate) fn write_private(path: &Path, content: &[u8]) -> Result<()> {
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let file = rustix::fs::open(path, flags, PRIVATE).map_err(raised)?;
    let mut file = File::from(file);
    file.write_all(content).map_err(|error| raised_by(&error))?;
    rustix::fs::fsync(&file).map_err(raised)
}

/// The document `data` is, read as the reference's reader read one: a byte
/// order mark of UTF-8 passed over, bytes that are no UTF-8 refused before
/// anything is read, and a record that names a field twice refused by the
/// class its reader raised.
pub(crate) fn decode(data: &[u8]) -> Result<Document> {
    let data = unmarked(data);
    require(std::str::from_utf8(data).is_ok(), "UnicodeDecodeError")?;
    Document::read(data).map_err(|unread| match unread {
        Unread::Duplicate(_) => Fault("DuplicateJsonError"),
        Unread::Invalid => Fault("JSONDecodeError"),
    })
}

/// The hash of a complete structured identity, whatever the order of its
/// records' fields: the SHA-256 of its fields in the order of their names,
/// with nothing between items.
pub fn digest(value: &Document) -> String {
    crate::digest(value.clone().sorted().compact().as_bytes())
}

/// The positive whole number that `value` is: no truth value, no fraction
/// and nothing missing.
pub fn positive(value: &Document) -> Result<u64> {
    let whole = match value {
        Document::Number(_) => value.whole(),
        _ => None,
    };
    let number = whole.filter(|number| *number > 0);
    let number = number.and_then(|number| u64::try_from(number).ok());
    number.ok_or(Fault("codeql_positive_integer"))
}

/// The field `name` of the record `record`.
pub(crate) fn field<'a>(record: &'a Document, name: &str) -> Result<&'a Document> {
    record.get(name).ok_or(KEY)
}

/// Whether the record `record` has the field `name`.
pub(crate) fn has(record: &Document, name: &str) -> bool {
    record.get(name).is_some()
}

/// The field `name` of `record`, or nothing where it has none.
pub(crate) fn get<'a>(record: &'a Document, name: &str) -> &'a Document {
    record.get(name).unwrap_or(&Document::Null)
}

/// The field `name` of `record`, or `default` where it has none.
pub(crate) fn get_or<'a>(record: &'a Document, name: &str, default: &'a Document) -> &'a Document {
    record.get(name).unwrap_or(default)
}

/// `value`, which must be a record.
pub(crate) fn object(value: &Document) -> Result<&Document> {
    match value {
        Document::Record(_) => Ok(value),
        _ => Err(VALUE),
    }
}

/// The items of `value`, which must be a list.
pub(crate) fn array(value: &Document) -> Result<&[Document]> {
    value.items().ok_or(VALUE)
}

/// The text `value` is.
pub(crate) fn string(value: &Document) -> Result<&str> {
    value.text().ok_or(VALUE)
}

/// The whole number `value` is, as the reference's reader took one: no
/// truth value, no fraction, and nothing that is no number.
pub(crate) fn integer(value: &Document) -> Result<i128> {
    match value {
        Document::Number(_) => value.whole().ok_or(VALUE),
        _ => Err(VALUE),
    }
}

/// Whether `value` is the text `text`.
pub(crate) fn same(value: &Document, text: &str) -> bool {
    value.text() == Some(text)
}

/// Whether `value` is one of the texts `choices`, as the reference asked a
/// set of them: a list or a record cannot be asked.
pub(crate) fn among(value: &Document, choices: &[&str]) -> Result<bool> {
    match value {
        Document::List(_) | Document::Record(_) => Err(UNHASHABLE),
        Document::Text(text) => Ok(choices.contains(&text.as_str())),
        _ => Ok(false),
    }
}

/// Whether the list `items` holds the text `text`.
pub(crate) fn holds(items: &[Document], text: &str) -> bool {
    items.iter().any(|item| same(item, text))
}

/// Whether `value` counts as something, as the reference's interpreter
/// counts a value: nothing, false, zero and an empty text, list or record
/// do not.
pub(crate) fn truthy(value: &Document) -> bool {
    match value {
        Document::Null => false,
        Document::Bool(held) => *held,
        Document::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Document::Text(text) => !text.is_empty(),
        Document::List(items) => !items.is_empty(),
        Document::Record(fields) => !fields.is_empty(),
    }
}

/// The categories of the stored security analyses every revision needs.
pub(crate) fn expected() -> Vec<String> {
    let languages = super::tools::STORED_LANGUAGES;
    languages
        .iter()
        .map(|language| format!("/language:{language}/security"))
        .collect()
}

#[cfg(test)]
#[path = "triage_properties.rs"]
mod properties;
#[cfg(test)]
#[path = "triage_tests.rs"]
pub(crate) mod tests;
