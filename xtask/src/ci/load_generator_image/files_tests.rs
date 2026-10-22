//! Retained namespace, special-mode, readback and exact-publication fixtures.
use super::super::tests::fixture::Fixture;
use super::*;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

#[test]
fn pre_admission_special_mode_and_changed_namespace_refuse_before_payload() {
    let fixture = Fixture::new();
    let error = Owned::new_with(&fixture.context, |path| {
        fs::set_permissions(path, fs::Permissions::from_mode(0o1700)).unwrap()
    });
    assert!(error.is_err());
    let files = fixture.files();
    assert!(
        files
            .create_with(
                Area::Work,
                "special",
                1024,
                || {},
                |file| file
                    .set_permissions(fs::Permissions::from_mode(0o4600))
                    .unwrap()
            )
            .is_err()
    );
    let parked = files.path.with_extension("parked");
    let visible = files.path.clone();
    assert!(
        files
            .create_with(
                Area::Work,
                "anchored",
                1024,
                || {
                    fs::rename(&visible, &parked).unwrap();
                    fs::create_dir(&visible).unwrap();
                },
                |_| {}
            )
            .is_err()
    );
    assert!(!visible.join("anchored").exists());
    assert!(parked.join("anchored").exists());
    assert!(files.finish().is_err());
}
#[test]
fn inputs_are_nofollow_single_link_bounded_and_initial_bindings_survive_replacement() {
    let fixture = Fixture::new();
    let files = fixture.files();
    let record = files.write(Area::Work, "input", b"original").unwrap();
    let path = files.archive_path(&record);
    fs::rename(&path, path.with_extension("retired")).unwrap();
    symlink("input.retired", &path).unwrap();
    assert!(files.read(&record).is_err());
    fs::remove_file(&path).unwrap();
    fs::write(&path, b"foreign").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(files.read(&record).is_err());
    files.finish().unwrap();
}
#[test]
fn publication_rechecks_exact_id_bytes_and_sealed_payload_before_any_rename() {
    for mutate_id in [false, true] {
        let fixture = Fixture::new();
        let mut files = fixture.files();
        let archive = files
            .write(
                Area::Output,
                &fixture.context.archive_name(),
                b"sealed compressed fixture",
            )
            .unwrap();
        let sealed = archive.stamp.clone();
        let id = files
            .write(
                Area::Output,
                "image-id.txt",
                format!("{}\n", fixture.image).as_bytes(),
            )
            .unwrap();
        let record = if mutate_id { &id } else { &archive };
        fs::write(files.archive_path(record), b"same-inode changed").unwrap();
        assert!(
            files
                .publish(&archive, &sealed, &id, &fixture.image)
                .is_err()
        );
        assert!(!fixture.root.join("temporary/loadtest-image").exists());
        files.finish().unwrap();
    }
}
#[test]
fn publication_never_replaces_foreign_destination_and_exact_success_has_only_two_files() {
    let fixture = Fixture::new();
    let mut files = fixture.files();
    let archive = files
        .write(
            Area::Output,
            &fixture.context.archive_name(),
            b"compressed fixture",
        )
        .unwrap();
    let sealed = archive.stamp.clone();
    let id = files
        .write(
            Area::Output,
            "image-id.txt",
            format!("{}\n", fixture.image).as_bytes(),
        )
        .unwrap();
    let foreign = fixture.root.join("temporary/loadtest-image");
    fs::create_dir(&foreign).unwrap();
    fs::write(foreign.join("foreign"), b"untouched").unwrap();
    assert!(
        files
            .publish(&archive, &sealed, &id, &fixture.image)
            .is_err()
    );
    assert_eq!(fs::read(foreign.join("foreign")).unwrap(), b"untouched");
    files.finish().unwrap();
}
#[test]
fn cache_absence_is_not_failed_or_unsafe_cache_admission() {
    let fixture = Fixture::new();
    let files = fixture.files();
    assert!(!files.cache().unwrap());
    let directory = fixture.context.temporary.join("buildx-cache");
    fs::create_dir(&directory).unwrap();
    assert!(!files.cache().unwrap());
    fs::write(directory.join("index.json"), b"{}").unwrap();
    assert!(files.cache().unwrap());
    fs::rename(directory.join("index.json"), directory.join("original")).unwrap();
    symlink("original", directory.join("index.json")).unwrap();
    assert!(files.cache().is_err());
    files.finish().unwrap();
}
