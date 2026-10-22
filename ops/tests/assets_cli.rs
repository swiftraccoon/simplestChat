//! Public CLI tests never acquire the real managed store, even under UID0.
use std::{fs, io::Cursor, process::Command};
#[test]
fn fixed_syntax_has_no_managed_root_owner_or_lock_bypass() {
    for args in [
        vec!["--root", "PRIVATE_ROOT"],
        vec!["initialize", "--owner", "0"],
        vec!["prune", "--revision", "PRIVATE_REVISION"],
        vec!["select"],
        vec!["publish", "--capture", "PRIVATE_REVISION"],
        vec!["initialize", "--skip-lock"],
        vec!["inspect", "--fixture", "PRIVATE_ROOT"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
            .args(["release", "assets"])
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let text = String::from_utf8(output.stderr).unwrap();
        assert!(!text.contains("PRIVATE"));
        assert!(text.contains("release assets initialize"));
    }
}
#[test]
fn capture_only_uses_an_owned_input_and_returns_no_names_or_contents() {
    let fixture = std::env::temp_dir().join(format!(
        "simplestchat-assets-cli-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&fixture).unwrap();
    let fixture = fixture.canonicalize().unwrap();
    let path = fixture.join("PRIVATE_CAPTURE.tar");
    let mut archive = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_path("assets/private-abcdef.js").unwrap();
    header.set_mode(0o644);
    header.set_size(7);
    header.set_cksum();
    archive.append(&header, Cursor::new(b"PRIVATE")).unwrap();
    fs::write(&path, archive.into_inner().unwrap()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
        .args(["release", "assets", "capture", "--path"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(
        output.stdout,
        b"{\"passed\":true,\"files\":1,\"bytes\":7}\n"
    );
    fs::write(&path, b"PRIVATE_INVALID_TAR").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
        .args(["release", "assets", "capture", "--path"])
        .arg(&path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["passed"], false);
    assert!(
        !String::from_utf8(output.stdout)
            .unwrap()
            .contains("PRIVATE")
    );
    fs::remove_dir_all(fixture).unwrap();
}
