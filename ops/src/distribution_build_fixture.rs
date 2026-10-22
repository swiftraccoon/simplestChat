//! Owned synthetic inputs only: these declarations are not a buildable crate.

use std::{fs, path::Path};

pub fn populate(root: &Path) -> Vec<(String, Vec<u8>)> {
    let paths = [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        ".cargo/config.toml",
        "ops/Cargo.toml",
        "load_tests/Cargo.toml",
        "xtask/Cargo.toml",
        "vendor/mediasoup-0.29.0/Cargo.toml",
        "vendor/mediasoup-sys-0.19.0/Cargo.toml",
        "ops/src/main.rs",
        "ops/src/release/mod.rs",
    ];
    paths
        .into_iter()
        .map(|path| {
            let bytes = format!("PRIVATE-INPUT/{path}\n").into_bytes();
            let target = root.join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, &bytes).unwrap();
            (path.into(), bytes)
        })
        .collect()
}
