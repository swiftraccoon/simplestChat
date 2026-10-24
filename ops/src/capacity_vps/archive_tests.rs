//! Real bounded tar/gzip writes over owned private files, with mature readers.

use super::*;
use crate::capacity_vps::{files::Access, host_tests::Temporary};
use std::{cell::Cell, io::Cursor, os::unix::fs::symlink, rc::Rc};

fn setup() -> (Temporary, Fs, Owner) {
    let temporary = Temporary::new();
    temporary.directory("/evidence", 0o700);
    temporary.directory("/output", 0o700);
    let fs = Fs::new(Access {
        root: Some(temporary.path("/")),
        hook: None,
    })
    .unwrap();
    let owner = Owner::fixture(Rc::new(Cell::new(super::super::control::now().unwrap())));
    (temporary, fs, owner)
}

#[test]
fn gzip_contains_exact_files_and_local_pax_for_a_long_path_without_extracting() {
    let (temporary, mut fs, mut owner) = setup();
    let name = format!("{}/{}.json", "é".repeat(80), "x".repeat(120));
    temporary.file("/evidence/request.json", b"original request bytes\n", 0o600);
    temporary.file(&format!("/evidence/{name}"), b"private long path\n", 0o600);
    let snapshot = Snapshot::walk(&mut fs, &mut owner, "/evidence").unwrap();
    assert_eq!(snapshot.count, 3);
    let id = fs.writer("/output/archive.gz").unwrap();
    let parent = fs.directory("/output", true).unwrap();
    let mut file = fs.take(id).unwrap();
    assert_eq!(fs.verify(), Err(Failure::Unproven));
    fs.trim_directories().unwrap();
    assert!(fs.dir_stamp(parent).unwrap().is_dir());
    let produced = produce(&snapshot, &mut fs, &mut owner, &mut file);
    file.sync_all().unwrap();
    fs.put(id, file).unwrap();
    fs.written(id).unwrap();
    fs.close(id).unwrap();
    produced.unwrap();
    snapshot.verify(&mut fs, &mut owner).unwrap();
    let bytes = std::fs::read(temporary.path("/output/archive.gz")).unwrap();
    let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut tar = tar::Archive::new(decoder);
    let mut members = std::collections::BTreeMap::new();
    for entry in tar.entries().unwrap() {
        let mut entry = entry.unwrap();
        assert!(entry.header().entry_type().is_file());
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        assert!(members.insert(path, bytes).is_none());
    }
    assert_eq!(members.len(), 2);
    assert_eq!(members["request.json"], b"original request bytes\n");
    assert_eq!(members[&name], b"private long path\n");
    fs.finish().unwrap();
    owner.finish().unwrap();
}

#[test]
fn archive_refuses_special_entries_and_same_bytes_replaced_after_snapshot() {
    let (temporary, mut fs, mut owner) = setup();
    temporary.file("/evidence/record", b"same", 0o600);
    let snapshot = Snapshot::walk(&mut fs, &mut owner, "/evidence").unwrap();
    std::fs::rename(
        temporary.path("/evidence/record"),
        temporary.path("/original"),
    )
    .unwrap();
    temporary.file("/evidence/record", b"same", 0o600);
    let id = fs.writer("/output/archive.gz").unwrap();
    let mut file = fs.take(id).unwrap();
    assert_eq!(
        produce(&snapshot, &mut fs, &mut owner, &mut file),
        Err(Failure::Changed)
    );
    fs.put(id, file).unwrap();
    fs.written(id).unwrap();
    fs.close(id).unwrap();
    let _ = fs.finish();
    assert_eq!(owner.finish().err(), Some(Failure::Changed));
    let (temporary, mut fs, mut owner) = setup();
    symlink("record", temporary.path("/evidence/link")).unwrap();
    assert!(matches!(
        Snapshot::walk(&mut fs, &mut owner, "/evidence"),
        Err(Failure::Archive)
    ));
    fs.finish().unwrap();
    owner.finish().unwrap();
}

#[test]
fn physical_entry_limit_counts_empty_directories_and_expanded_limit_counts_padding() {
    let (temporary, mut fs, mut owner) = setup();
    // Independent empty directories consume the same physical namespace bound
    // as files, even though the producer emits only regular evidence members.
    for n in 0..=MAX_ENTRIES {
        temporary.directory(&format!("/evidence/d{n}"), 0o700);
    }
    assert!(matches!(
        Snapshot::walk(&mut fs, &mut owner, "/evidence"),
        Err(Failure::Bounds)
    ));
    fs.finish().unwrap();
    owner.finish().unwrap();
    let mut owner = Owner::fixture(Rc::new(Cell::new(super::super::control::now().unwrap())));
    let mut bytes = Vec::new();
    let mut expanded = Expanded {
        inner: &mut bytes,
        count: MAX_EXPANDED - 512,
        owner: &mut owner,
    };
    expanded.write_all(&[0u8; 512]).unwrap();
    assert!(expanded.write_all(&[0u8; 512]).is_err());
    assert_eq!(bytes.len(), 512);
    owner.finish().unwrap();
}
