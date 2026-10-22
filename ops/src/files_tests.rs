use super::*;
use crate::test_support::Fixture;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

#[test]
fn owned_regular_inputs_are_bounded_and_links_never_resolve() {
    let root = Fixture::new();
    let file = root.0.join("manifest.json");
    fs::write(&file, b"private").unwrap();
    assert_eq!(read(&file, 7).unwrap(), b"private");
    assert_eq!(read(&file, 6).unwrap_err(), Error::Input);
    assert_eq!(read(&root.0, 7).unwrap_err(), Error::Input);
    assert_eq!(read(&root.0.join("absent"), 7).unwrap_err(), Error::Input);
    let link = root.0.join("link");
    symlink(&file, &link).unwrap();
    assert_eq!(read(&link, 7).unwrap_err(), Error::Input);
    let parent = root.0.join("parent-link");
    symlink(&root.0, &parent).unwrap();
    assert_eq!(
        read(&parent.join("manifest.json"), 7).unwrap_err(),
        Error::Input
    );
    assert_eq!(
        read(&root.0.join("../manifest.json"), 7).unwrap_err(),
        Error::Input
    );
    let fifo = root.0.join("fifo");
    nix::unistd::mkfifo(&fifo, Mode::from_bits_truncate(0o600)).unwrap();
    assert_eq!(read(&fifo, 7).unwrap_err(), Error::Input);
    fs::write(&file, []).unwrap();
    assert_eq!(read(&file, 7).unwrap_err(), Error::Input);
}

#[test]
fn replacement_same_length_writes_and_mode_changes_refuse_success() {
    let root = Fixture::new();
    let file = root.0.join("manifest.json");
    for operation in ["replace", "write", "chmod", "grow", "truncate", "unlink"] {
        fs::write(&file, b"private").unwrap();
        let result = read_after(&file, 10, || match operation {
            "replace" => {
                let next = root.0.join("replacement");
                fs::write(&next, b"PRIVATE").unwrap();
                fs::rename(next, &file).unwrap();
            }
            "write" => fs::write(&file, b"PRIVATE").unwrap(),
            "chmod" => fs::set_permissions(&file, fs::Permissions::from_mode(0o400)).unwrap(),
            "grow" => fs::write(&file, b"private more").unwrap(),
            "truncate" => fs::write(&file, []).unwrap(),
            "unlink" => fs::remove_file(&file).unwrap(),
            _ => unreachable!(),
        });
        assert_eq!(result.unwrap_err(), Error::Changed, "{operation}");
        if file.exists() {
            fs::remove_file(&file).unwrap();
        }
    }
}

#[test]
fn ancestor_replacement_is_detected_without_reading_outside_the_retained_directory() {
    let root = Fixture::new();
    let directory = root.0.join("owned");
    fs::create_dir(&directory).unwrap();
    let file = directory.join("manifest.json");
    fs::write(&file, b"private").unwrap();
    let result = read_after(&file, 7, || {
        fs::rename(&directory, root.0.join("retired")).unwrap();
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("manifest.json"), b"FOREIGN").unwrap();
    });
    assert_eq!(result.unwrap_err(), Error::Changed);
    assert_eq!(
        fs::read(directory.join("manifest.json")).unwrap(),
        b"FOREIGN"
    );
    assert_eq!(
        fs::read(root.0.join("retired/manifest.json")).unwrap(),
        b"private"
    );
}
