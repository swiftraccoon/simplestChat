//! Real owned files demonstrate that the copied install input is authenticated.
use super::*;
use crate::ci::install_compose::tests::{CONTENT, DIGEST, Fixture};
use std::fs;

#[test]
fn altering_final_private_copy_before_metadata_seal_refuses_install_input() {
    let fixture = Fixture::new();
    let mut owned = Owned::new(&fixture.context).unwrap();
    owned.prepare(&fixture.context).unwrap();
    let mut output = open_at(
        &owned.directory,
        "verified",
        OFlags::RDWR | OFlags::CREATE | OFlags::EXCL,
    )
    .unwrap();
    output.write_all(CONTENT).unwrap();
    output.sync_all().unwrap();
    // This models a change after the source copy/hash completed, before the
    // final output metadata is obtained. Metadata alone would trust these bytes.
    let mut other = open_at(&owned.directory, "verified", OFlags::WRONLY).unwrap();
    other.write_all(b"changed").unwrap();
    other.sync_all().unwrap();
    close(other).unwrap();
    assert!(authenticate_copy(&owned.directory, &mut output, DIGEST).is_err());
    close(output).unwrap();
    let (reporter, cleanup) = owned.cleanup();
    cleanup.unwrap();
    reporter.write(&serde_json::json!({})).unwrap();
}

#[test]
fn renamed_runner_temp_cannot_redirect_final_evidence_or_privileged_source_path() {
    let fixture = Fixture::new();
    let mut owned = Owned::new(&fixture.context).unwrap();
    owned.prepare(&fixture.context).unwrap();
    let name = owned.report_name.clone();
    let original = fixture.root.join("original-parent");
    fs::rename(&fixture.context.temporary, &original).unwrap();
    fs::create_dir(&fixture.context.temporary).unwrap();
    fs::write(fixture.context.temporary.join(&name), b"foreign sentinel").unwrap();
    assert!(owned.check_directory().is_err());
    let (reporter, cleanup) = owned.cleanup();
    cleanup.unwrap();
    reporter
        .write(&serde_json::json!({"passed":false,"failure":"parent_changed"}))
        .unwrap();
    assert_eq!(
        fs::read(fixture.context.temporary.join(&name)).unwrap(),
        b"foreign sentinel"
    );
    assert!(original.join(&name).is_file());
}

#[test]
fn report_creation_is_exclusive_on_original_parent_descriptor() {
    let fixture = Fixture::new();
    let owned = Owned::new(&fixture.context).unwrap();
    let path = fixture.context.temporary.join(&owned.report_name);
    fs::write(&path, b"unchanged").unwrap();
    let (reporter, cleanup) = owned.cleanup();
    cleanup.unwrap();
    assert!(reporter.write(&serde_json::json!({"passed":true})).is_err());
    assert_eq!(fs::read(path).unwrap(), b"unchanged");
}
