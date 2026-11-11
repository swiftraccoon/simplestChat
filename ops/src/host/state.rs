//! Private state files: bounded strict reads and durable replacement.

use super::failure::{Failure, Result, require};
use crate::json::{self, Json};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// Read at most `limit` bytes of one file.
pub(crate) fn read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let bytes = std::fs::read(path)?;
    if bytes.len() > limit {
        return Err(Failure::Value);
    }
    Ok(bytes)
}

/// Decode strict JSON: duplicate keys, non-finite numbers and trailing text
/// are not values.
pub(crate) fn decode(bytes: &[u8]) -> Result<Json> {
    json::decode_limited(bytes, bytes.len().max(1)).map_err(|_| Failure::Value)
}

/// The object in `path`, or an empty one when the file does not exist.
pub(crate) fn object_or_empty(path: &Path, limit: usize) -> Result<Json> {
    if !path.exists() {
        return decode(b"{}");
    }
    let value = decode(&read(path, limit)?)?;
    value.object().map(|_| value.clone()).ok_or(Failure::Value)
}

/// Require an ordinary path owned by `owner` with one of `modes`, and at most
/// `limit` bytes when a limit is given. Links are not followed.
pub(crate) fn protected(
    path: &Path,
    owner: u32,
    directory: bool,
    modes: &[u32],
    limit: Option<u64>,
) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    require(
        metadata.uid() == owner && modes.contains(&(metadata.permissions().mode() & 0o7777)),
        "Unexpected ownership or permissions",
    )?;
    require(
        if directory {
            metadata.is_dir()
        } else {
            metadata.is_file()
        },
        "Unexpected file type",
    )?;
    require(
        limit.is_none_or(|limit| metadata.len() <= limit),
        "Oversized configuration",
    )
}

/// Who owns a replaced file, and its mode.
pub(crate) struct Access {
    pub owner: u32,
    pub group: u32,
    pub mode: u32,
}

/// Replace `path` durably with `bytes`: a private sibling is written and
/// synced, renamed over the target, and the directory is synced.
pub(crate) fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    replace(path, bytes, None)
}

/// Replace `path` as `atomic` does, giving the new file to `access` before
/// it takes the name.
pub(crate) fn atomic_for(path: &Path, bytes: &[u8], access: &Access) -> Result<()> {
    replace(path, bytes, Some(access))
}

fn replace(path: &Path, bytes: &[u8], access: Option<&Access>) -> Result<()> {
    let parent = path.parent().ok_or(Failure::Os)?;
    let mut random = [0_u8; 8];
    getrandom::fill(&mut random).map_err(|_| Failure::Os)?;
    let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let temporary = parent.join(format!(".release-{suffix}"));
    let written = (|| -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        let mut file = options
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(bytes)?;
        if let Some(access) = access {
            std::os::unix::fs::fchown(&file, Some(access.owner), Some(access.group))?;
            file.set_permissions(PermissionsExt::from_mode(access.mode))?;
        }
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
        return written;
    }
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// Replace `path` with `value` as indented JSON and a final newline.
pub(crate) fn atomic_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut text = serde_json::to_string_pretty(value).map_err(|_| Failure::Value)?;
    text.push('\n');
    atomic(path, text.as_bytes())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::{Path, PathBuf};

    /// A private directory under the system temporary directory, removed on drop.
    pub(crate) struct Scratch(pub PathBuf);

    impl Scratch {
        pub(crate) fn new() -> Self {
            let mut random = [0_u8; 8];
            getrandom::fill(&mut random).unwrap();
            let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
            let path = std::env::temp_dir().join(format!("simplestchat-monitor-{suffix}"));
            std::fs::create_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }

        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A replacement leaves exactly the new bytes, private, and no sibling.
    #[test]
    fn atomic_replacement_is_private_complete_and_leaves_no_sibling() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let path = scratch.path().join("state.json");
        super::atomic(&path, b"first").unwrap();
        super::atomic_json(&path, &serde_json::json!({"a": 1})).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"{\n  \"a\": 1\n}\n");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 1);
        let missing = scratch.path().join("absent/state.json");
        assert!(super::atomic(&missing, b"x").is_err());
    }
}
