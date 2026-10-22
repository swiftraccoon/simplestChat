//! Owned filesystem races and private writer admission; no service is run.

use super::{files::Directory, tests::Fixture};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

#[test]
fn fresh_private_output_refuses_existing_leaves_and_linked_ancestors() {
    let fixture = Fixture::new();
    let path = fixture.0.join("fresh");
    let output = Directory::new(&path).unwrap();
    assert!(Directory::new(&path).is_err());
    symlink(&fixture.0, fixture.0.join("linked")).unwrap();
    assert!(Directory::new(&fixture.0.join("linked/escape")).is_err());
    assert!(!fixture.0.join("escape").exists());
    output.finish().unwrap();
}

#[test]
fn replacement_credentials_are_not_deleted_or_adopted_as_owned_inputs() {
    let fixture = Fixture::new();
    let path = fixture.0.join("fresh");
    let output = Directory::new(&path).unwrap();
    output
        .json(
            "accounts.json",
            &serde_json::json!([{"password":"private"}]),
        )
        .unwrap();
    fs::rename(path.join("accounts.json"), path.join("original.json")).unwrap();
    fs::write(path.join("accounts.json"), b"foreign private file").unwrap();
    fs::set_permissions(
        path.join("accounts.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(output.read("accounts.json", 1024).is_err());
    assert!(output.remove(&["accounts.json"]).is_err());
    assert_eq!(
        fs::read(path.join("accounts.json")).unwrap(),
        b"foreign private file"
    );
    output.finish().unwrap();
}

#[test]
fn mode_namespace_and_single_link_changes_refuse_positive_close_or_read() {
    for kind in ["file-mode", "directory-mode", "hardlink", "namespace"] {
        let fixture = Fixture::new();
        let path = fixture.0.join("fresh");
        let output = Directory::new(&path).unwrap();
        let mut record = output.create("record.json", 16).unwrap();
        record.append(b"bounded").unwrap();
        match kind {
            "file-mode" => {
                fs::set_permissions(path.join("record.json"), fs::Permissions::from_mode(0o644))
                    .unwrap()
            }
            "directory-mode" => {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap()
            }
            "hardlink" => fs::hard_link(path.join("record.json"), path.join("alias")).unwrap(),
            _ => {
                fs::rename(&path, fixture.0.join("moved")).unwrap();
                fs::create_dir(&path).unwrap();
            }
        }
        assert!(record.finish(&output).is_err(), "{kind}");
        assert!(output.read("record.json", 16).is_err(), "{kind}");
        let _ = output.finish();
    }
}

#[test]
fn private_files_enforce_the_actual_written_byte_cap_and_exclusive_names() {
    let fixture = Fixture::new();
    let output = Directory::new(&fixture.0.join("fresh")).unwrap();
    let mut record = output.create("bounded", 4).unwrap();
    record.append(b"1234").unwrap();
    assert!(record.append(b"5").is_err());
    assert!(output.create("bounded", 4).is_err());
    record.finish(&output).unwrap();
    assert_eq!(output.read("bounded", 4).unwrap(), b"1234");
    output.remove(&["bounded", ".creator", "absent"]).unwrap();
    output.remove_directory().unwrap();
}

#[test]
fn same_length_mutation_before_the_written_stamp_cannot_be_adopted() {
    let fixture = Fixture::new();
    let path = fixture.0.join("fresh");
    let output = Directory::new(&path).unwrap();
    let mut record = output.create("fixed.json", 64).unwrap();
    record.append(b"original").unwrap();
    fs::write(path.join("fixed.json"), b"modified").unwrap();
    assert!(record.finish(&output).is_err());
    output.finish().unwrap();
}
