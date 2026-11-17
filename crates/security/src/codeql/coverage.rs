//! The proof that a native extraction saw the sources it must: the query
//! result that lists every file the compiler was observed compiling must
//! name each of the worker's protocol parsers, whatever the compiler's
//! exit status said.

use super::unmarked;
use crate::{Fault, Result, files, paths, require};
use serde_json::Value;
use std::{collections::BTreeSet, ffi::OsStr, io::Write, path::Path};

/// The sources an extraction must have compiled.
pub const REQUIRED: [&str; 4] = [
    "vendor/mediasoup-sys-0.19.0/src/RTC/DtlsTransport.cpp",
    "vendor/mediasoup-sys-0.19.0/src/RTC/ICE/StunPacket.cpp",
    "vendor/mediasoup-sys-0.19.0/src/RTC/SCTP/association/Association.cpp",
    "vendor/mediasoup-sys-0.19.0/src/RTC/RTP/Packet.cpp",
];
/// The most bytes of the query result.
pub const MAX_EVIDENCE: u64 = 4 * 1024 * 1024;
/// The most characters of one compiled file's name.
const MAX_NAME: usize = 2048;
/// What the command says when the evidence is refused.
const REFUSED: &str = "Native CodeQL extraction evidence failed validation.\n";

/// The field `name` of the record `value`.
fn field<'a>(value: &'a Value, name: &str) -> Result<&'a Value> {
    let record = value.as_object().ok_or(Fault("scanner_object_expected"))?;
    record.get(name).ok_or(Fault("codeql_coverage_field"))
}

/// The list `value`.
fn list(value: &Value) -> Result<&[Value]> {
    let items = value.as_array().ok_or(Fault("scanner_list_expected"))?;
    Ok(items)
}

/// How many distinct files the query result at `path` lists, once it is
/// read whole within its bound as the reference read JSON: a field named
/// twice counts as given last. Every row is one name, and the required
/// sources are among them.
pub fn verify(path: &Path) -> Result<usize> {
    let data = files::bounded(path, MAX_EVIDENCE, false)?;
    let raw: Value =
        serde_json::from_slice(unmarked(&data)).map_err(|_| Fault("codeql_coverage_json"))?;
    let table = field(&raw, "#select")?;
    let mut compiled = BTreeSet::new();
    for value in list(field(table, "tuples")?)? {
        let row = list(value)?;
        require(row.len() == 1, "codeql_coverage_columns")?;
        let name = row[0].as_str().filter(|name| {
            let length = name.chars().count();
            length > 0 && length <= MAX_NAME
        });
        compiled.insert(name.ok_or(Fault("invalid_tool_string"))?);
    }
    let complete = REQUIRED.iter().all(|name| compiled.contains(name));
    require(complete, "codeql_native_extraction_incomplete")?;
    Ok(compiled.len())
}

/// The exit status of `codeql coverage` with `arguments`: the number of
/// compiled files printed for the one query result named, or one fixed
/// line on `err`. Neither a source nor a query result is ever printed.
pub fn command(arguments: &[String], out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let counted = match arguments {
        [path] => verify(&paths::path(OsStr::new(path))),
        _ => Err(Fault("codeql_coverage_argument")),
    };
    match counted {
        Ok(count) => {
            let line = format!("Verified native CodeQL extraction: {count} compiled files.\n");
            match out.write_all(line.as_bytes()) {
                Ok(()) => 0,
                Err(_) => 1,
            }
        }
        Err(_) => {
            let _ = err.write_all(REFUSED.as_bytes());
            1
        }
    }
}

#[cfg(test)]
#[path = "coverage_tests.rs"]
mod tests;
