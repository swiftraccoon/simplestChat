//! The actual offline archive command against owned, inert metadata/layer files.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::fs;
use std::io::Cursor;
use std::os::unix::fs::{DirBuilderExt, symlink};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "simplestchat-archive-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn fixture() -> (Fixture, Vec<u8>, Vec<u8>) {
    let root = Fixture::new();
    let revision = "a".repeat(40);
    let tag = format!("simplestchat-release/production:{revision}");
    let config = json!({"os":"linux","architecture":"amd64","config":{"User":"10001:10001",
        "Labels":{"org.opencontainers.image.revision":revision},"Cmd":["/app/simplestChat"],
        "Env":["PRIVATE_SECRET=never_print"]}})
    .to_string();
    let mut archive = tar::Builder::new(Vec::new());
    for (name, bytes) in [
        (
            "manifest.json",
            json!([{"Config":"config.json","RepoTags":[tag],"Layers":["layer.tar"]}])
                .to_string()
                .into_bytes(),
        ),
        ("config.json", config.clone().into_bytes()),
        ("layer.tar", b"inert fixture, never executed".to_vec()),
    ] {
        let mut header = tar::Header::new_ustar();
        header.set_path(name).unwrap();
        header.set_mode(0o600);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        archive.append(&header, Cursor::new(bytes)).unwrap();
    }
    let bytes = archive.into_inner().unwrap();
    let manifest = json!({"schemaVersion":1,"revision":revision,"platform":"linux/amd64",
        "archiveSha256":digest(&bytes),"imageTag":tag,"migrations":{"1":"c".repeat(96)},
        "createdAt":"2026-10-21T01:02:03Z"});
    fs::write(root.0.join("release.json"), manifest.to_string()).unwrap();
    fs::write(root.0.join("image.tar"), &bytes).unwrap();
    (root, bytes, config.into_bytes())
}
fn cli(args: &[&OsStr]) -> Output {
    // No helper executable is available through PATH. Validation must stay local.
    Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
        .args(args)
        .env("PATH", "")
        .output()
        .unwrap()
}
fn archive(root: &Fixture, path: &std::path::Path) -> Output {
    cli(&[
        "release".as_ref(),
        "archive".as_ref(),
        "--manifest".as_ref(),
        root.0.join("release.json").as_os_str(),
        "--path".as_ref(),
        path.as_os_str(),
    ])
}
#[test]
fn success_binds_transfer_and_config_but_never_prints_config_or_claims_authorization() {
    let (root, bytes, config) = fixture();
    let path = root.0.join("image.tar");
    let output = archive(&root, &path);
    assert!(output.status.success(), "{:?}", output.stderr);
    assert!(output.stderr.is_empty());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result,
        json!({"archiveSha256":digest(&bytes),"archiveBytes":bytes.len(),
        "configSha256":digest(&config),
        "revision":"a".repeat(40),
        "imageTag":format!("simplestchat-release/production:{}","a".repeat(40)),
        "platform":"linux/amd64"})
    );
    assert!(
        !String::from_utf8(output.stdout)
            .unwrap()
            .contains("PRIVATE_SECRET")
    );
    assert!(result.get("passed").is_none());
    assert!(result.get("attestation").is_none());
    assert_eq!(fs::read(path).unwrap(), bytes);
}
#[test]
fn missing_changed_and_nonregular_inputs_have_one_fixed_private_failure() {
    let (root, _, _) = fixture();
    let original = root.0.join("image.tar");
    let link = root.0.join("PRIVATE_PATH.tar");
    symlink(&original, &link).unwrap();
    let parent = root.0.join("PRIVATE_PARENT");
    symlink(&root.0, &parent).unwrap();
    let fifo = root.0.join("fifo");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
    for path in [
        &link,
        &parent.join("image.tar"),
        &fifo,
        &root.0,
        &root.0.join("missing"),
    ] {
        let output = archive(&root, path);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stderr.is_empty());
        assert_eq!(
            output.stdout,
            b"{\"passed\":false,\"failure\":\"invalid_release_archive\"}\n"
        );
    }
    fs::write(&original, b"PRIVATE_ARCHIVE not a tar").unwrap();
    let output = archive(&root, &original);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        output.stdout,
        b"{\"passed\":false,\"failure\":\"invalid_release_archive\"}\n"
    );
    fs::write(root.0.join("release.json"), b"{\"PRIVATE_SECRET\":NaN}").unwrap();
    let output = archive(&root, &original);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    assert_eq!(
        output.stdout,
        b"{\"passed\":false,\"failure\":\"invalid_release_archive\"}\n"
    );
}
#[test]
fn arguments_are_fixed_and_do_not_select_another_operation() {
    for args in [
        vec!["release", "archive"],
        vec![
            "release",
            "archive",
            "--path",
            "private",
            "--manifest",
            "private",
        ],
        vec![
            "release",
            "archive",
            "--manifest",
            "private",
            "--path",
            "private",
            "extra",
        ],
        vec![
            "release",
            "archive",
            "--manifest",
            "private",
            "--path",
            "private",
            "--extract",
        ],
    ] {
        let output = cli(&args.iter().map(OsStr::new).collect::<Vec<_>>());
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert_eq!(
            output.stderr,
            concat!(
                "Usage: simplestchat-ops release manifest --path PATH\n",
                "       simplestchat-ops release archive --manifest PATH --path PATH\n",
                "       simplestchat-ops release receipt --envelope PATH --receipt PATH\n",
                "       simplestchat-ops distribution inspect-executable --path PATH\n",
                "       simplestchat-ops distribution verify-pair --manifest PATH ",
                "--path PATH --selection PATH\n",
                "       simplestchat-ops distribution build-plan ",
                "--selection PATH --source-root PATH\n",
            )
            .as_bytes()
        );
    }
}
