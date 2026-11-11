//! Private local files: owned by the caller, bounded, never reached through
//! a symbolic link, and created exclusively.

use super::{Refusal, Result, require};
use rustix::fs::OFlags;
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// The largest private file the controller reads.
pub(super) const MAX_PRIVATE_BYTES: u64 = 65536;

/// A local failure that is not one of the named refusals.
pub(super) fn metadata<T>(_: T) -> Refusal {
    Refusal("invalid_local_or_remote_metadata")
}

fn nofollow() -> i32 {
    // The flag set is a small positive constant on every supported platform.
    i32::try_from(OFlags::NOFOLLOW.bits()).unwrap()
}

fn caller() -> u32 {
    rustix::process::getuid().as_raw()
}

/// `value` as an absolute path whose final component is not a symbolic link
/// and whose directory is resolved; the file itself need not exist yet.
pub(super) fn local_path(value: &str, home: &Path, current: &Path) -> Result<PathBuf> {
    let path = match value.strip_prefix("~/") {
        Some(tail) => home.join(tail),
        None if value == "~" => home.to_owned(),
        None => current.join(value),
    };
    let linked = path.symlink_metadata();
    let linked = linked.is_ok_and(|found| found.file_type().is_symlink());
    require(!linked, "selected_path_is_symlink")?;
    let name = path.file_name().ok_or(Refusal("invalid_local_path"))?;
    let parent = path.parent().ok_or(Refusal("invalid_local_path"))?;
    Ok(parent.canonicalize().map_err(metadata)?.join(name))
}

/// Private authentication material stays entirely outside the checkout.
pub(super) fn outside_checkout(path: &Path, root: &Path) -> Result<()> {
    let root = root.canonicalize().map_err(metadata)?;
    require(!path.starts_with(root), "credential_path_inside_checkout")
}

/// The bytes of a bounded regular file the caller owns: mode `0600` exactly
/// when `secret`, otherwise merely not writable by group or others.
pub(super) fn protected_file(path: &Path, secret: bool) -> Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    let file = options.read(true).custom_flags(nofollow()).open(path);
    let mut file = file.map_err(metadata)?;
    let found = file.metadata().map_err(metadata)?;
    require(found.is_file(), "invalid_private_file_type")?;
    require(found.uid() == caller(), "invalid_private_file_owner")?;
    let mode = found.mode() & 0o7777;
    let private = if secret {
        mode == 0o600
    } else {
        mode & 0o022 == 0
    };
    require(private, "invalid_private_file_mode")?;
    let sized = found.len() > 0 && found.len() <= MAX_PRIVATE_BYTES;
    require(sized, "invalid_private_file_size")?;
    let mut bytes = Vec::new();
    let mut bounded = (&mut file).take(MAX_PRIVATE_BYTES + 1);
    bounded.read_to_end(&mut bytes).map_err(metadata)?;
    let read = u64::try_from(bytes.len()).map_err(metadata)?;
    require(read <= MAX_PRIVATE_BYTES, "invalid_private_file_size")?;
    Ok(bytes)
}

/// The directory of `path` exists, belongs to the caller and cannot be
/// modified by anyone else.
pub(super) fn private_parent(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or(Refusal("invalid_private_parent"))?;
    let found = parent.metadata().map_err(metadata)?;
    require(found.is_dir(), "invalid_private_parent")?;
    require(found.uid() == caller(), "invalid_private_parent_owner")?;
    require(found.mode() & 0o022 == 0, "unsafe_private_parent_mode")
}

/// Makes a newly published directory entry durable.
fn sync_parent(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or(Refusal("invalid_private_parent"))?;
    let directory = File::open(parent).map_err(metadata)?;
    directory.sync_all().map_err(metadata)
}

/// Creates a private file exclusively; an existing file is never replaced.
pub(super) fn write_new(path: &Path, data: &[u8]) -> Result<()> {
    private_parent(path)?;
    let mut options = OpenOptions::new();
    let options = options.write(true).create_new(true).mode(0o600);
    let mut file = options
        .custom_flags(nofollow())
        .open(path)
        .map_err(metadata)?;
    file.write_all(data).map_err(metadata)?;
    file.sync_all().map_err(metadata)?;
    drop(file);
    sync_parent(path)
}

/// Flushes a validated key file and its directory entry.
pub(super) fn sync_file(path: &Path) -> Result<()> {
    let mut options = OpenOptions::new();
    let file = options.read(true).custom_flags(nofollow()).open(path);
    let file = file.map_err(metadata)?;
    require(
        file.metadata().map_err(metadata)?.is_file(),
        "invalid_key_type",
    )?;
    file.sync_all().map_err(metadata)?;
    sync_parent(path)
}

/// Retains a fresh evidence record with private permissions.
pub(super) fn write_record(path: &Path, value: &Value) -> Result<()> {
    let mut text = serde_json::to_string_pretty(value).map_err(metadata)?;
    text.push('\n');
    write_new(path, text.as_bytes())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A private directory under the system temporary directory, removed on
    /// drop, with its symbolic links resolved as the controller resolves them.
    pub(in super::super) struct Scratch(pub PathBuf);

    impl Scratch {
        pub fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let next = NEXT.fetch_add(1, Ordering::Relaxed);
            let name = format!("simplestchat-bootstrap-{}-{next}", std::process::id());
            let path = std::env::temp_dir().canonicalize().unwrap().join(name);
            std::fs::create_dir(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }

        pub fn file(&self, name: &str, data: &[u8], mode: u32) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, data).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn code<T: std::fmt::Debug>(result: Result<T>) -> &'static str {
        result.unwrap_err().0
    }

    /// From `test_private_files_reject_permissions_symlinks_and_checkout`.
    #[test]
    fn private_files_reject_permissions_symlinks_and_checkout() {
        let scratch = Scratch::new();
        let secret = scratch.file("secret", b"value\n", 0o600);
        assert_eq!(protected_file(&secret, true).unwrap(), b"value\n");
        let readable = scratch.file("readable", b"value\n", 0o644);
        assert_eq!(
            code(protected_file(&readable, true)),
            "invalid_private_file_mode"
        );
        assert_eq!(protected_file(&readable, false).unwrap(), b"value\n");
        let writable = scratch.file("writable", b"value\n", 0o664);
        assert_eq!(
            code(protected_file(&writable, false)),
            "invalid_private_file_mode"
        );
        let empty = scratch.file("empty", b"", 0o600);
        assert_eq!(
            code(protected_file(&empty, true)),
            "invalid_private_file_size"
        );
        let large = vec![b'x'; usize::try_from(MAX_PRIVATE_BYTES).unwrap() + 1];
        let large = scratch.file("large", &large, 0o600);
        assert_eq!(
            code(protected_file(&large, true)),
            "invalid_private_file_size"
        );
        let link = scratch.0.join("link");
        symlink(&secret, &link).unwrap();
        assert_eq!(
            code(protected_file(&link, true)),
            "invalid_local_or_remote_metadata"
        );
        let home = scratch.0.clone();
        assert_eq!(
            code(local_path("link", &home, &scratch.0)),
            "selected_path_is_symlink"
        );
        assert_eq!(
            local_path("~/secret", &home, Path::new("/")).unwrap(),
            secret
        );
        assert_eq!(local_path("secret", &home, &scratch.0).unwrap(), secret);
        assert_eq!(
            code(outside_checkout(&secret, &scratch.0)),
            "credential_path_inside_checkout"
        );
        let elsewhere = Scratch::new();
        outside_checkout(&secret, &elsewhere.0).unwrap();
    }

    /// A record is created once with private permissions and never replaced,
    /// and a parent that others can modify is refused.
    #[test]
    fn records_are_exclusive_private_and_need_a_private_parent() {
        let scratch = Scratch::new();
        let path = scratch.0.join("record.json");
        write_record(&path, &serde_json::json!({"completed": true, "port": 22})).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "{\n  \"completed\": true,\n  \"port\": 22\n}\n");
        assert_eq!(path.metadata().unwrap().mode() & 0o7777, 0o600);
        assert_eq!(
            code(write_new(&path, b"again")),
            "invalid_local_or_remote_metadata"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        sync_file(&path).unwrap();
        let shared = scratch.0.join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(
            code(write_new(&shared.join("file"), b"x")),
            "unsafe_private_parent_mode"
        );
        assert!(!shared.join("file").exists());
    }
}
