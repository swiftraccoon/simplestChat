//! Local new-contract pairs only: no signature, candidate or host execution.

#[path = "../src/distribution_fixture.rs"]
mod elf;
#[path = "../src/distribution_manifest_fixture.rs"]
mod fixture;
use serde_json::Value;
use std::{
    ffi::OsStr,
    fs,
    os::unix::fs::{DirBuilderExt, symlink},
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "simplestchat-pair-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        let binary = elf::fixture(true, false, false);
        fs::write(path.join("candidate"), &binary).unwrap();
        fs::write(
            path.join("ops-release.json"),
            serde_json::to_vec_pretty(&fixture::manifest(&binary)).unwrap(),
        )
        .unwrap();
        fs::write(
            path.join("selection.json"),
            serde_json::to_vec(&fixture::selection()).unwrap(),
        )
        .unwrap();
        Self(path)
    }
    fn run(&self) -> Output {
        cli(&[
            "distribution".as_ref(),
            "verify-pair".as_ref(),
            "--manifest".as_ref(),
            self.0.join("ops-release.json").as_os_str(),
            "--path".as_ref(),
            self.0.join("candidate").as_os_str(),
            "--selection".as_ref(),
            self.0.join("selection.json").as_os_str(),
        ])
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
fn refused(output: Output) {
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    assert_eq!(
        output.stdout,
        b"{\"passed\":false,\"failure\":\"invalid_ops_artifact\"}\n"
    );
}

#[test]
fn pair_projection_binds_original_bytes_and_shows_claims_separately_from_facts() {
    let root = Fixture::new();
    let path = root.0.join("ops-release.json");
    let binary = fs::read(root.0.join("candidate")).unwrap();
    let text = serde_json::to_string_pretty(&fixture::manifest(&binary)).unwrap();
    for original in [
        text.as_bytes().to_vec(),
        text.encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect::<Vec<_>>(),
    ] {
        fs::write(&path, &original).unwrap();
        let output = root.run();
        assert!(output.status.success(), "{:?}", output.stderr);
        assert!(output.stderr.is_empty());
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["manifestSha256"], fixture::digest(&original));
        assert_eq!(value["manifestBytes"], original.len());
        assert_eq!(value["claims"]["artifactType"], "simplestchat-ops");
        assert_eq!(value["claims"]["repository"], "owner/repo");
        assert_eq!(value["claims"]["evidenceDigestCount"], 0);
        assert_eq!(value["executable"]["sha256"], fixture::digest(&binary));
        assert_eq!(value["executable"]["noInterpreter"], true);
        assert_eq!(value["executable"]["noNeeded"], true);
        let public = String::from_utf8(output.stdout).unwrap();
        for hidden in [
            "PRIVATE-SENTINEL",
            "inputDigests",
            "evidenceDigests",
            "passed",
            "signed",
        ] {
            assert!(!public.contains(hidden));
        }
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read(root.0.join("candidate")).unwrap(), binary);
    }
}

#[test]
fn wrong_selected_source_scope_or_binary_never_becomes_success() {
    for (file, bytes) in [
        ("ops-release.json", b"{\"PRIVATE_SECRET\":NaN}".to_vec()),
        ("selection.json", b"{}".to_vec()),
        ("candidate", b"PRIVATE_SECRET invalid candidate".to_vec()),
    ] {
        let root = Fixture::new();
        fs::write(root.0.join(file), bytes).unwrap();
        refused(root.run());
    }
    for field in [
        "repository",
        "revision",
        "packageVersion",
        "buildRunId",
        "runAttempt",
    ] {
        let root = Fixture::new();
        let mut selection = fixture::selection();
        selection[field] = match field {
            "repository" => "other/repo".into(),
            "revision" => "b".repeat(40).into(),
            "packageVersion" => "0.1.1".into(),
            _ => 999.into(),
        };
        fs::write(
            root.0.join("selection.json"),
            serde_json::to_vec(&selection).unwrap(),
        )
        .unwrap();
        refused(root.run());
    }
}

#[test]
fn all_three_inputs_refuse_links_special_files_and_size_violations_privately() {
    for file in ["ops-release.json", "selection.json", "candidate"] {
        for kind in ["symlink", "fifo", "oversize"] {
            let root = Fixture::new();
            let path = root.0.join(file);
            if kind == "oversize" {
                let limit = match file {
                    "ops-release.json" => 65536,
                    "selection.json" => 16384,
                    _ => 256 * 1024 * 1024,
                };
                fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_len(limit + 1)
                    .unwrap();
            } else {
                fs::remove_file(&path).unwrap();
                if kind == "symlink" {
                    symlink("PRIVATE-PATH", &path).unwrap();
                } else {
                    nix::unistd::mkfifo(&path, nix::sys::stat::Mode::from_bits_truncate(0o600))
                        .unwrap();
                }
            }
            refused(root.run());
        }
    }
}

#[test]
fn fixed_argument_order_cannot_select_installation_or_any_other_operation() {
    for args in [
        vec!["distribution", "verify-pair"],
        vec![
            "distribution",
            "verify-pair",
            "--path",
            "PRIVATE",
            "--manifest",
            "PRIVATE",
            "--selection",
            "PRIVATE",
        ],
        vec![
            "distribution",
            "install",
            "--manifest",
            "PRIVATE",
            "--path",
            "PRIVATE",
            "--selection",
            "PRIVATE",
        ],
    ] {
        let output = cli(&args.iter().map(OsStr::new).collect::<Vec<_>>());
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(
            !String::from_utf8(output.stderr)
                .unwrap()
                .contains("PRIVATE")
        );
    }
}
