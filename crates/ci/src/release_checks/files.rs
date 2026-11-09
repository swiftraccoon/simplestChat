//! Release-check selections over retained executable identities: the first
//! PATH Node and the discovered Chromium, whose received path never appears
//! in diagnostics.

use simplestchat_toolkit::{
    executable::{self, Executable, MAX_PATH, invalid},
    tooling::{Environment, Failure, Result},
};
use std::path::Path;

pub(super) fn failure(message: &str) -> Failure {
    Failure::new(1, message)
}

/// Match the shell's first executable PATH candidate; no ambient PATH fallback.
pub(super) fn node(root: &Path, environment: &Environment) -> Result<Executable> {
    executable::select(root, environment, std::ffi::OsStr::new("node"))
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
