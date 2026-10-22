use super::*;
use crate::bench::restart_browser::fixtures::Fixture;
use serde_json::json;
use std::os::fd::AsRawFd;

#[test]
fn a_changed_source_refuses_and_closes_every_acquired_file() {
    let fixture = Fixture::new(json!({}));
    let sources = Sources::open(&fixture.root).unwrap();
    let descriptors = sources
        .0
        .iter()
        .map(|v| v.file.as_ref().unwrap().as_raw_fd())
        .collect::<Vec<_>>();
    for descriptor in &descriptors {
        assert!(std::fs::metadata(format!("/dev/fd/{descriptor}")).is_ok());
    }
    std::fs::write(fixture.root.join(NAMES[2]), b"// changed source bytes\n").unwrap();
    assert!(sources.finish().is_err());
    for descriptor in descriptors {
        assert!(std::fs::metadata(format!("/dev/fd/{descriptor}")).is_err());
    }
}
#[test]
fn same_bytes_replaced_inode_and_symlink_are_refused() {
    let fixture = Fixture::new(json!({}));
    let sources = Sources::open(&fixture.root).unwrap();
    let path = fixture.root.join(NAMES[0]);
    std::fs::rename(&path, path.with_extension("held")).unwrap();
    std::fs::write(&path, b"// inert selected source\n").unwrap();
    assert!(sources.check().is_err());
    assert!(sources.finish().is_err());
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(path.with_extension("held"), &path).unwrap();
    assert!(Sources::open(&fixture.root).is_err());
}
