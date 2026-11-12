//! `simplestchat-ops release ledger`: require the database's migration
//! ledger to be exactly the migrations of the verified source checkout a
//! launcher is deploying. The launcher gives the ledger as the database
//! printed it, one `version success checksum` row per line; every row must
//! have succeeded, and the rows must be the checkout's migrations with their
//! SHA-384 digests, no more and no fewer.

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod tests;

use crate::host::fault::Fault;
use crate::host::{emit, hexadecimal};
use crate::release::admit::migrations;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Where a launcher keeps the verified source checkouts, by revision.
const SOURCES: &str = "/srv/simplestchat-bench/sources";
const MAX_LEDGER: usize = 1024 * 1024;
const UNMET: &str = "Operational precondition failed";
const DIFFERS: &str = "Migration ledger differs from the verified source image";
const MATCHES: &str = "Packaged migration checksums match.\n";

/// The rows of a printed ledger, by version. A row that did not succeed, a
/// version given twice or any other shape is not a ledger.
fn rows(ledger: &[u8]) -> Result<BTreeMap<String, String>, &'static str> {
    let text = std::str::from_utf8(ledger).map_err(|_| UNMET)?;
    let mut found = BTreeMap::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [version, "t", checksum] = fields[..] else {
            return Err(UNMET);
        };
        // A version as the database prints it: a positive decimal number.
        let canonical = !version.starts_with('0')
            && version.bytes().all(|byte| byte.is_ascii_digit())
            && version.parse::<i64>().is_ok();
        if !canonical
            || found
                .insert(version.to_owned(), checksum.to_owned())
                .is_some()
        {
            return Err(UNMET);
        }
    }
    Ok(found)
}

/// Compare the ledger with the migrations of the checkout of `revision`
/// under `sources`.
fn judge(sources: &Path, revision: &str, ledger: &[u8]) -> Result<(), &'static str> {
    let expected = match migrations(&sources.join(revision)) {
        Ok(expected) => expected,
        // A checkout without a migration matches no ledger.
        Err(Fault::Refused(code)) if code == "invalid_migration_count" => BTreeMap::new(),
        Err(_) => return Err(UNMET),
    };
    let recorded = rows(ledger)?;
    match !expected.is_empty() && recorded == expected {
        true => Ok(()),
        false => Err(DIFFERS),
    }
}

/// `--revision REVISION --ledger PATH`, or nothing for any other command
/// line. A revision is a full commit identity; it names a directory.
fn parse(arguments: &[OsString]) -> Option<(&str, PathBuf)> {
    let [first, revision, second, ledger] = arguments else {
        return None;
    };
    let revision = revision
        .to_str()
        .filter(|revision| hexadecimal(revision, 40))?;
    (first == "--revision" && second == "--ledger").then(|| (revision, PathBuf::from(ledger)))
}

/// Run `release ledger --revision REVISION --ledger PATH`. A syntax error
/// returns 2 before anything is read; a ledger that matches prints one line
/// and returns success; anything else names why on standard error.
pub fn run(arguments: &[OsString]) -> ExitCode {
    let Some((revision, path)) = parse(arguments) else {
        return ExitCode::from(2);
    };
    let ledger = crate::host::state::read(&path, MAX_LEDGER).map_err(|_| UNMET);
    let judged = ledger.and_then(|ledger| judge(Path::new(SOURCES), revision, &ledger));
    match judged {
        Ok(()) if emit(std::io::stdout(), MATCHES) => ExitCode::SUCCESS,
        Ok(()) => ExitCode::FAILURE,
        Err(reason) => {
            let _ = emit(std::io::stderr(), &format!("{reason}\n"));
            ExitCode::FAILURE
        }
    }
}
