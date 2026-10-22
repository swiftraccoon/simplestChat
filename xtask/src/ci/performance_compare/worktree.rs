//! Bounded Git registration observations and nofollow vendor timestamp updates.

use super::{files, plan};
use crate::tooling::Result;
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, Timespec, Timestamps};
use std::{collections::BTreeSet, fs::File, path::Path};

pub(super) fn revision(bytes: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(bytes).map_err(|_| plan::failure(1))?;
    let text = text.strip_suffix('\n').ok_or_else(|| plan::failure(1))?;
    if text.len() != 40
        || !text
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
    {
        return Err(plan::failure(1));
    }
    Ok(text.into())
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Registration {
    Absent,
    Owned,
}

/// `--porcelain -z` keeps paths as data. Only this invocation's exact leaf may
/// authorize removal; a successful complete listing supplies absence evidence.
pub(super) fn listing(bytes: &[u8], path: &Path, revision: &str) -> Result<Registration> {
    if bytes.is_empty() || bytes.len() > 4 * 1024 * 1024 || !bytes.ends_with(&[0, 0]) {
        return Err(plan::failure(1));
    }
    let path = path.to_str().ok_or_else(|| plan::failure(1))?;
    let mut current: Option<&str> = None;
    let mut head = None;
    let mut detached = false;
    let mut branch = false;
    let mut bare = false;
    let mut blocked = false;
    let mut seen = false;
    let mut populated = false;
    let mut count = 0;
    let mut paths = BTreeSet::new();
    for token in bytes.split(|v| *v == 0) {
        if token.is_empty() {
            if populated
                && (current.is_none()
                    || (!bare && (head.is_none() || !(detached || branch)))
                    || (detached && branch)
                    || (bare && (head.is_some() || detached || branch)))
            {
                return Err(plan::failure(1));
            }
            if current == Some(path) {
                if seen || head != Some(revision) || !detached || blocked {
                    return Err(plan::failure(1));
                }
                seen = true;
            }
            current = None;
            head = None;
            detached = false;
            branch = false;
            bare = false;
            blocked = false;
            populated = false;
            continue;
        }
        count += 1;
        if count > 100_000 {
            return Err(plan::failure(1));
        }
        let text = std::str::from_utf8(token).map_err(|_| plan::failure(1))?;
        if let Some(value) = text.strip_prefix("worktree ") {
            if populated
                || !Path::new(value).is_absolute()
                || value.len() > 4096
                || !paths.insert(value)
            {
                return Err(plan::failure(1));
            }
            current = Some(value);
        } else if let Some(value) = text.strip_prefix("HEAD ") {
            if current.is_none()
                || value.len() != 40
                || !value
                    .bytes()
                    .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
                || head.replace(value).is_some()
            {
                return Err(plan::failure(1));
            }
        } else if current.is_none() {
            return Err(plan::failure(1));
        } else if text == "detached" {
            if detached {
                return Err(plan::failure(1));
            }
            detached = true;
        } else if text == "locked"
            || text.starts_with("locked ")
            || text == "prunable"
            || text.starts_with("prunable ")
        {
            blocked = true;
        } else if text == "bare" {
            if bare {
                return Err(plan::failure(1));
            }
            bare = true;
        } else if let Some(value) = text.strip_prefix("branch ") {
            if branch || value.is_empty() {
                return Err(plan::failure(1));
            }
            branch = true;
        } else {
            return Err(plan::failure(1));
        }
        populated = true;
    }
    Ok(if seen {
        Registration::Owned
    } else {
        Registration::Absent
    })
}

/// The hosted action's UTC 2000 timestamp, applied to links themselves just as
/// `find vendor -exec touch -h ...` did. No link target is traversed or written.
pub(super) fn vendor_times(root: &files::Scope) -> Result<()> {
    root.check()?;
    let vendor = rustix::fs::openat(
        root.file(),
        "vendor",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|_| plan::failure(1))?;
    let result = walk(&vendor, 0, &mut 0, &mut 0);
    let closed = files::close(vendor);
    result?;
    closed?;
    touch(root.file(), c"vendor")?;
    root.check()
}
fn touch(parent: &File, name: &std::ffi::CStr) -> Result<()> {
    let times = Timestamps {
        last_access: Timespec {
            tv_sec: 946_684_800,
            tv_nsec: 0,
        },
        last_modification: Timespec {
            tv_sec: 946_684_800,
            tv_nsec: 0,
        },
    };
    rustix::fs::utimensat(parent, name, &times, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| plan::failure(1))
}
fn walk(root: &File, depth: usize, count: &mut usize, names: &mut usize) -> Result<()> {
    if depth > 64 {
        return Err(plan::failure(1));
    }
    let entries = Dir::read_from(root).map_err(|_| plan::failure(1))?;
    for entry in entries {
        let entry = entry.map_err(|_| plan::failure(1))?;
        let name = entry.file_name();
        if [b".".as_slice(), b".."].contains(&name.to_bytes()) {
            continue;
        }
        *count += 1;
        *names += name.to_bytes().len();
        if *count > 100_000 || *names > 16 * 1024 * 1024 {
            return Err(plan::failure(1));
        }
        let stat = rustix::fs::statat(root, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| plan::failure(1))?;
        match FileType::from_raw_mode(stat.st_mode) {
            FileType::Directory => {
                let child = rustix::fs::openat(
                    root,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map(File::from)
                .map_err(|_| plan::failure(1))?;
                let result = walk(&child, depth + 1, count, names);
                let closed = files::close(child);
                result?;
                closed?;
            }
            FileType::RegularFile | FileType::Symlink => {}
            _ => return Err(plan::failure(1)),
        }
        touch(root, name)?;
    }
    Ok(())
}
