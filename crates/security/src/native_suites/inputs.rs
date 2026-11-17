//! The inputs a suite's image is built from: the worker's maintained
//! sources, the image's recipe and the reviewed data, and the digest that
//! binds them.

use super::{corpus::corpus, read_regular};
use crate::{Fault, Result, require};
use sha2::{Digest as _, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

/// The worker's directory.
pub const WORKER: &str = "vendor/mediasoup-sys-0.19.0";
/// The maintained parts of the worker that its build reads, as the Rust
/// build's allowlist names them.
const SOURCE_PATHS: [&str; 19] = [
    "deps/libwebrtc",
    "fbs",
    "fuzzer/include",
    "fuzzer/src",
    "mocks/include",
    "mocks/src",
    "include",
    "scripts",
    "src",
    "subprojects/packagefiles",
    "test/include",
    "test/src",
    "build.rs",
    "Cargo.toml",
    "meson.build",
    "meson_options.txt",
    "python-invoke-requirements.txt",
    "python-tools-requirements.txt",
    "tasks.py",
];
/// What the worker generates inside those parts, which is no input.
const EXCLUDED: [&str; 2] = ["include/FBS", "scripts/node_modules"];
/// The image's recipe and the reviewed data beside the worker's sources.
const RECIPE: [&str; 5] = [
    "build/native-security.Dockerfile",
    "build/install-openssl.sh",
    "build/pip-constraints.txt",
    "security/native/toolchain.json",
    "security/native/corpus.json",
];
/// The most files an image is built from.
const MAX_SOURCE_FILES: usize = 20_000;
/// The most bytes of one of them.
const MAX_SOURCE_BYTES: u64 = 32 << 20;

/// Every regular file below `directory`, never through a link, but for what
/// `excluded` says the worker generates.
fn walked(worker: &Path, directory: &Path, selected: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries: Vec<PathBuf> = fs::read_dir(directory)
        .map_err(|_| Fault("missing_native_source"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<_>>()
        .map_err(|_| Fault("missing_native_source"))?;
    entries.sort();
    for path in entries {
        let local = path
            .strip_prefix(worker)
            .map_err(|_| Fault("missing_native_source"))?;
        let local = local.to_string_lossy();
        let skipped = EXCLUDED
            .iter()
            .any(|excluded| local == *excluded || local.starts_with(&format!("{excluded}/")));
        if skipped {
            continue;
        }
        let metadata = path
            .symlink_metadata()
            .map_err(|_| Fault("missing_native_source"))?;
        require(!metadata.file_type().is_symlink(), "native_source_symlink")?;
        if metadata.is_dir() {
            walked(worker, &path, selected)?;
        } else {
            require(metadata.is_file(), "native_source_not_regular")?;
            selected.push(path);
        }
    }
    Ok(())
}

/// The files below `root` an image is built from, in order: the worker's
/// maintained sources and its wraps, the recipe and the reviewed corpus.
pub fn source_files(root: &Path) -> Result<Vec<PathBuf>> {
    let worker = root.join(WORKER);
    let mut selected = Vec::new();
    for relative in SOURCE_PATHS {
        let path = worker.join(relative);
        let metadata = path.symlink_metadata();
        let present = metadata
            .as_ref()
            .is_ok_and(|metadata| !metadata.file_type().is_symlink());
        require(present, "missing_native_source")?;
        if metadata.is_ok_and(|metadata| metadata.is_dir()) {
            walked(&worker, &path, &mut selected)?;
        } else {
            require(path.is_file(), "native_source_not_regular")?;
            selected.push(path);
        }
    }
    let wraps =
        fs::read_dir(worker.join("subprojects")).map_err(|_| Fault("missing_native_source"))?;
    for entry in wraps {
        let path = entry.map_err(|_| Fault("missing_native_source"))?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "wrap")
        {
            selected.push(path);
        }
    }
    selected.extend(RECIPE.iter().map(|relative| root.join(relative)));
    let cases = corpus(root)?;
    let native = root.join("security/native");
    selected.extend(cases.iter().map(|case| native.join(&case.relative)));
    require(
        (1..=MAX_SOURCE_FILES).contains(&selected.len()),
        "native_source_count",
    )?;
    selected.sort();
    Ok(selected)
}

/// The digest of every input below `root`: each file's path relative to
/// `root`, a zero byte, and its bytes' SHA-256, in order. With a
/// `destination`, the same bytes are copied there, an executable one kept
/// executable by its owner alone, and nothing else.
pub fn inputs_digest(root: &Path, destination: Option<&Path>) -> Result<String> {
    let mut hashed = Sha256::new();
    for path in source_files(root)? {
        let relative = path
            .strip_prefix(root)
            .map_err(|_| Fault("missing_native_source"))?;
        let data = read_regular(&path, MAX_SOURCE_BYTES)?;
        hashed.update(relative.to_string_lossy().as_bytes());
        hashed.update([0]);
        hashed.update(Sha256::digest(&data));
        if let Some(destination) = destination {
            let target = destination.join(relative);
            let parent = target.parent().ok_or(Fault("missing_native_source"))?;
            fs::create_dir_all(parent).map_err(|_| Fault("native_context_unwritable"))?;
            crate::files::write_private(&target, &data, 0o600)?;
            let mode = path
                .metadata()
                .map_err(|_| Fault("missing_native_source"))?
                .permissions()
                .mode();
            if mode & 0o111 != 0 {
                let executable = fs::Permissions::from_mode(0o700);
                fs::set_permissions(&target, executable)
                    .map_err(|_| Fault("native_context_unwritable"))?;
            }
        }
    }
    Ok(crate::hexadecimal(&hashed.finalize()))
}
