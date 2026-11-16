//! What the image gate's scanners read and write. A tree the gate stages
//! for a scanner is opened to an account that does not own it, read only;
//! what a scanner writes is held to a budget of entries and bytes while it
//! runs. Neither follows a link, and what the walk cannot examine counts
//! against the budget instead of being passed over.

use crate::{Result, document::Document, files, require};
use std::{
    ffi::OsString,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

/// How much of its budget an output uses, counted up to the first entry
/// that is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
    /// Whether the output is within its budget.
    pub within: bool,
    /// The bytes of the regular files counted.
    pub bytes: u64,
    /// The entries counted.
    pub entries: u64,
    /// Whether an entry is neither a directory nor a regular file, or
    /// could not be examined.
    pub rejected: bool,
}

impl Usage {
    /// The usage as the evidence records it.
    pub fn record(&self) -> Document {
        Document::record([
            ("withinLimit", self.within.into()),
            ("observedRegularBytes", self.bytes.into()),
            ("observedEntries", self.entries.into()),
            ("rejectedType", self.rejected.into()),
        ])
    }
}

/// The entries of `directory` in the order of their names: those that are
/// directories, a final link followed, then the others.
fn listed(directory: &Path) -> std::io::Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let mut names = Vec::<OsString>::new();
    for entry in fs::read_dir(directory)? {
        names.push(entry?.file_name());
    }
    names.sort();
    let paths = names.into_iter().map(|name| directory.join(name));
    Ok(paths.partition(|path| path.is_dir()))
}

/// How much of a budget of `limit` bytes and `entries` entries the tree at
/// `path` uses. Each directory's entries are counted before those of the
/// directories below it, its directories before its other entries and each
/// kind in the order of their names, up to the first entry that is neither
/// a directory nor a regular file, cannot be examined or passes a bound. A
/// directory that cannot be listed is refused as such an entry is: the
/// scanner writes as the gate's own account, and could hide what it wrote
/// in one.
pub fn usage(path: &Path, limit: u64, entries: u64) -> Usage {
    let mut counted = Usage {
        within: true,
        bytes: 0,
        entries: 0,
        rejected: false,
    };
    let refused = |counted: Usage| Usage {
        within: false,
        rejected: true,
        ..counted
    };
    let mut pending = vec![path.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok((directories, others)) = listed(&directory) else {
            return refused(counted);
        };
        for entry in directories.iter().chain(&others) {
            counted.entries += 1;
            let Ok(kind) = entry.symlink_metadata() else {
                return refused(counted);
            };
            if !(kind.is_dir() || kind.is_file()) {
                return refused(counted);
            }
            if kind.is_file() {
                counted.bytes = counted.bytes.saturating_add(kind.len());
            }
            if counted.entries > entries || counted.bytes > limit {
                return Usage {
                    within: false,
                    ..counted
                };
            }
        }
        // A link among them was refused above: these are directories.
        pending.extend(directories.into_iter().rev());
    }
    counted
}

/// Open the staging tree at `path`, which the gate made, to a scanner that
/// runs as another account and only to read: each file becomes 0444 and
/// each directory 0555, the files of a directory before its directories and
/// a directory before what it holds, and the tree's own directory last. A
/// link anywhere is refused, and what was opened before it stays open.
pub fn readable(path: &Path) -> Result<()> {
    let mut pending = vec![path.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let (directories, others) = listed(&directory).map_err(|error| files::unusable(&error))?;
        for (entries, mode) in [(&others, 0o444), (&directories, 0o555)] {
            for entry in entries {
                let kind = entry
                    .symlink_metadata()
                    .map_err(|error| files::unusable(&error))?;
                require(!kind.file_type().is_symlink(), "image_scanner_input_link")?;
                opened(entry, mode)?;
            }
        }
        pending.extend(directories.into_iter().rev());
    }
    opened(path, 0o555)
}

/// Give `path` the permissions `mode`.
fn opened(path: &Path, mode: u32) -> Result<()> {
    let permissions = fs::Permissions::from_mode(mode);
    fs::set_permissions(path, permissions).map_err(|error| files::unusable(&error))
}

#[cfg(test)]
#[path = "outputs_tests.rs"]
mod tests;
