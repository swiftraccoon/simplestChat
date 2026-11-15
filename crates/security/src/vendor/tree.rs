//! The vendored directory as it is on disk: every file whole, with no link
//! followed and nothing but directories and regular files in it.

use super::{Bounds, Verdict, compare::Files, manifest::relative_path, problem};
use rustix::fs::{Mode, OFlags};
use std::{
    fs::{File, Metadata},
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

/// The whole of the regular file `path` of at most `limit` bytes, read
/// through no link, and unchanged while it was read.
pub(crate) fn read_regular(path: &Path, limit: u64) -> Verdict<Vec<u8>> {
    let unreadable = || problem(format!("Unreadable file: {}", path.display()));
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let opened = rustix::fs::open(path, flags, Mode::empty());
    let file = opened.map(File::from).map_err(|_| unreadable())?;
    let before = file.metadata().map_err(|_| unreadable())?;
    if !before.is_file() || before.len() > limit {
        return Err(problem(format!(
            "Not a bounded regular file: {}",
            path.display()
        )));
    }
    let mut data = Vec::new();
    let read = (&file).take(limit + 1).read_to_end(&mut data);
    read.map_err(|_| unreadable())?;
    let after = file.metadata().map_err(|_| unreadable())?;
    let identity = |held: &Metadata| {
        let (modified, changed) = (held.mtime_nsec(), held.ctime_nsec());
        (held.len(), held.mtime(), modified, held.ctime(), changed)
    };
    if data.len() as u64 > limit || identity(&before) != identity(&after) {
        return Err(problem(format!(
            "File changed or exceeded its limit: {}",
            path.display()
        )));
    }
    Ok(data)
}

/// The entries of `directory`: its directories, as whatever a name leads
/// to, and everything else, both in the order of their names.
fn entries(directory: &Path) -> Verdict<(Vec<PathBuf>, Vec<PathBuf>)> {
    let unreadable = || {
        problem(format!(
            "Unreadable vendor directory: {}",
            directory.display()
        ))
    };
    let (mut directories, mut others) = (Vec::new(), Vec::new());
    for entry in std::fs::read_dir(directory).map_err(|_| unreadable())? {
        let path = entry.map_err(|_| unreadable())?.path();
        if path.is_dir() {
            directories.push(path);
        } else {
            others.push(path);
        }
    }
    directories.sort();
    others.sort();
    Ok((directories, others))
}

/// Every file below the directory `root`, by its path from there.
pub fn files(root: &Path, bounds: &Bounds) -> Verdict<Files> {
    let real = |path: &Path| path.symlink_metadata().is_ok_and(|held| held.is_dir());
    if !real(root) {
        return Err(problem(format!(
            "Not a source directory: {}",
            root.display()
        )));
    }
    let mut found = Files::new();
    let (mut total, mut seen) = (0_u64, 0_usize);
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let (directories, others) = entries(&directory)?;
        seen += directories.len() + others.len();
        if seen > bounds.entries {
            return Err(problem("Too many vendor filesystem entries"));
        }
        if !directories.iter().all(|path| real(path)) {
            return Err(problem("Symlink or special vendor directory"));
        }
        for path in others {
            let below = path.strip_prefix(root).unwrap_or(&path);
            let key = relative_path(&below.to_string_lossy())?.to_owned();
            let data = read_regular(&path, bounds.member)?;
            total += data.len() as u64;
            if total > bounds.content {
                return Err(problem("Vendor content budget exceeded"));
            }
            found.insert(key, data);
        }
        pending.extend(directories.into_iter().rev());
    }
    Ok(found)
}

#[cfg(test)]
#[path = "tree_tests.rs"]
mod tests;
