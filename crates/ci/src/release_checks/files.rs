//! Release-check selections over retained executable identities: the first
//! PATH Node and the discovered Chromium, whose received path never appears
//! in diagnostics; and the step's own private output files.

use simplestchat_toolkit::{
    executable::{self, Executable, MAX_PATH, invalid},
    tooling::{Environment, Failure, Result},
};
use std::{
    fs::File,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};

pub(super) fn failure(message: &str) -> Failure {
    Failure::new(1, message)
}

/// Match the shell's first executable PATH candidate; no ambient PATH fallback.
pub(super) fn node(root: &Path, environment: &Environment) -> Result<Executable> {
    executable::select(root, environment, std::ffi::OsStr::new("node"))
}

/// Create the new private directory `path`, refusing any existing entry.
pub(super) fn private_directory(path: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new().mode(0o700).create(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

/// Create `path` as a new empty file of mode 0600.
pub(super) fn create_private(path: &Path) -> std::io::Result<File> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

/// Create `path` with exactly `bytes` and a final newline.
pub(super) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = create_private(path)?;
    file.write_all(bytes)?;
    file.write_all(b"\n")
}

pub(super) fn chromium(bytes: &[u8]) -> Result<Executable> {
    if bytes.is_empty() || bytes.len() > MAX_PATH {
        return Err(invalid());
    }
    let path = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    if path.chars().any(char::is_control) {
        return Err(invalid());
    }
    Executable::open(Path::new(path))
}

#[cfg(test)]
mod resolver_tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    #[test]
    fn node_delegation_keeps_first_executable_path_and_ordinary_symlink_selection() {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("shared-node-resolution-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let first = root.join("first");
        let second = root.join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let executable = second.join("actual node");
        fs::write(&executable, b"owned-inert").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(first.join("node"), b"not executable").unwrap();
        fs::set_permissions(first.join("node"), fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&executable, second.join("node")).unwrap();
        let environment = Environment::from([(
            "PATH".into(),
            std::env::join_paths([first, second]).unwrap(),
        )]);
        let selected = node(&root, &environment).unwrap();
        assert_eq!(selected.validate().unwrap(), executable);
        selected.close().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
