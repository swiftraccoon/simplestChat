//! The native worker's finite security suites: its reviewed corpus replayed
//! through the fuzzer target and its sanitizer tests run, each in an offline
//! sandbox of a local engine whose image is built from exactly the inputs it
//! binds. These are local checks only; no automated run may start them.

pub mod budget;
pub mod cli;
pub mod compiled;
pub mod corpus;
pub mod engine;
pub mod inputs;
pub mod local;
pub mod run;
pub mod sandbox;

use crate::{Fault, Result, require};
use rustix::fs::{Mode, OFlags};
use std::{fs::File, io::Read, path::Path};

/// The parser families of the worker, each of the corpus's inputs replayed
/// by one.
pub const FAMILIES: [&str; 7] = ["stun", "dtls", "sctp", "rtp", "rtcp", "codecs", "utils"];
/// The suites a run may be of.
pub const MODES: [&str; 3] = ["asan", "ubsan", "replay"];
/// One mebibyte.
pub(crate) const MIB: u64 = 1 << 20;
/// A file that cannot be opened or read as it stands, a final link among
/// them.
const UNOPENED: Fault = Fault("native_input_unreadable");

/// The whole of the regular file `path`, of at most `limit` bytes, read
/// without following a final link, and refused if it changes while it is
/// read.
pub(crate) fn read_regular(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let file = rustix::fs::open(path, flags, Mode::empty());
    let file = file.map(File::from).map_err(|_| UNOPENED)?;
    let metadata = file.metadata().map_err(|_| UNOPENED)?;
    require(metadata.is_file(), "input_not_regular")?;
    require(metadata.len() <= limit, "input_size_exceeded")?;
    let mut content = Vec::new();
    let read = file.take(limit.saturating_add(1)).read_to_end(&mut content);
    read.map_err(|_| UNOPENED)?;
    require(content.len() as u64 == metadata.len(), "input_changed")?;
    Ok(content)
}

/// Whether `text` is `digits` small hexadecimal digits.
pub(crate) fn hexadecimal(text: &str, digits: usize) -> bool {
    let digit = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
    text.len() == digits && text.bytes().all(digit)
}

#[cfg(test)]
#[path = "native_suites/tests.rs"]
pub(crate) mod tests;
