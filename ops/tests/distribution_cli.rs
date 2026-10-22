//! Execute only the inspector, with metadata-only candidates and an empty PATH.

#[path = "../src/distribution_fixture.rs"]
mod fixture;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    fs,
    os::unix::fs::{DirBuilderExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "simplestchat-executable-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn cli(args: &[&OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
        .args(args)
        .env("PATH", "")
        .output()
        .unwrap()
}
fn inspect(path: &Path) -> Output {
    cli(&[
        "distribution".as_ref(),
        "inspect-executable".as_ref(),
        "--path".as_ref(),
        path.as_os_str(),
    ])
}

#[test]
fn original_identity_and_linkage_facts_never_print_candidate_strings_or_provenance_claims() {
    let root = Fixture::new();
    let path = root.0.join("PRIVATE-PATH");
    for (dynamic, interpreter, needed) in [
        (false, false, false),
        (true, false, false),
        (true, true, true),
    ] {
        let mut bytes = fixture::fixture(dynamic, interpreter, needed);
        bytes.extend_from_slice(b"PRIVATE-SENTINEL-overlay");
        fs::write(&path, &bytes).unwrap();
        let result = inspect(&path);
        assert!(result.status.success());
        assert!(result.stderr.is_empty());
        let value: Value = serde_json::from_slice(&result.stdout).unwrap();
        let digest: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(value["sha256"], digest);
        assert_eq!(value["bytes"], bytes.len());
        assert_eq!(value["architecture"], "x86_64");
        assert_eq!(value["noInterpreter"], !interpreter);
        assert_eq!(value["noNeeded"], !needed);
        assert_eq!(value["neededCount"], usize::from(needed));
        assert_eq!(value["pieFlag"], dynamic);
        let output = String::from_utf8(result.stdout).unwrap();
        for private in ["PRIVATE", "musl", "compatible", "attestation", "passed"] {
            assert!(!output.contains(private));
        }
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn malformed_files_links_special_files_and_sizes_have_one_fixed_private_failure() {
    let root = Fixture::new();
    let path = root.0.join("PRIVATE-PATH");
    fs::write(&path, b"PRIVATE-SENTINEL").unwrap();
    let link = root.0.join("link");
    symlink(&path, &link).unwrap();
    let fifo = root.0.join("fifo");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
    for path in [&path, &link, &fifo, &root.0, &root.0.join("missing")] {
        let output = inspect(path);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stderr.is_empty());
        assert_eq!(
            output.stdout,
            b"{\"passed\":false,\"failure\":\"invalid_executable\"}\n"
        );
    }
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(256 * 1024 * 1024 + 1)
        .unwrap();
    assert_eq!(inspect(&path).status.code(), Some(1));
}

#[test]
fn argument_forms_cannot_execute_a_candidate_or_choose_transport() {
    for args in [
        vec!["distribution"],
        vec!["distribution", "inspect-executable", "--path"],
        vec![
            "distribution",
            "inspect-executable",
            "--wrong",
            "PRIVATE-PATH",
        ],
        vec![
            "distribution",
            "inspect-executable",
            "--path",
            "PRIVATE-PATH",
            "extra",
        ],
        vec!["distribution", "install", "--path", "PRIVATE-PATH"],
    ] {
        let result = cli(&args.iter().map(OsStr::new).collect::<Vec<_>>());
        assert_eq!(result.status.code(), Some(2));
        assert!(result.stdout.is_empty());
        assert!(
            !String::from_utf8(result.stderr)
                .unwrap()
                .contains("PRIVATE-PATH")
        );
    }
}
