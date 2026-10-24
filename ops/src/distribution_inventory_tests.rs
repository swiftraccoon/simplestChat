//! Small owned namespace/race/bound fixtures; no compiler or candidate runs.

use super::super::fixture;
use super::*;
use crate::test_support::Fixture;
use std::{fs, os::unix::fs::symlink};

fn source() -> (Fixture, std::path::PathBuf) {
    let root = Fixture::new();
    let path = root.0.join("source");
    fixture::populate(&path);
    (root, path)
}

#[test]
fn framed_inventory_binds_sorted_names_sizes_and_original_bytes() {
    let root = Fixture::new();
    let path = root.0.join("source");
    let mut expected = fixture::populate(&path);
    expected.sort_by(|a, b| a.0.cmp(&b.0));
    let mut frame = b"simplestchat-ops-capacity-source-inventory-v2\0".to_vec();
    frame.extend((expected.len() as u32).to_be_bytes());
    for (name, bytes) in &expected {
        frame.extend((name.len() as u16).to_be_bytes());
        frame.extend(name.as_bytes());
        frame.extend((bytes.len() as u64).to_be_bytes());
        frame.extend(Sha256::digest(bytes));
    }
    let inventory = collect_inputs(&path).unwrap();
    let digest: String = Sha256::digest(frame)
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect();
    assert_eq!(inventory.sha256(), digest);
    assert_eq!(inventory.files(), expected.len());
    assert_eq!(
        inventory.bytes(),
        expected.iter().map(|(_, v)| v.len() as u64).sum::<u64>()
    );
    assert_eq!(inventory, collect_inputs(&path).unwrap());
    let public = format!("{inventory:?}");
    assert!(!public.contains("PRIVATE-INPUT"));
    assert!(!public.contains("Cargo.toml"));
    assert!(!public.contains(path.to_str().unwrap()));
    for file in FIXED {
        let old = collect_inputs(&path).unwrap();
        fs::write(path.join(file), b"changed\n").unwrap();
        assert_ne!(old.sha256(), collect_inputs(&path).unwrap().sha256());
    }
}

#[test]
fn retained_file_namespace_and_directory_membership_changes_refuse_success() {
    for kind in ["write", "replace", "membership", "directory", "root"] {
        let (_root, path) = source();
        let result = collect_after(&path, |_| match kind {
            "write" => fs::write(path.join("ops/src/main.rs"), b"replacement").unwrap(),
            "replace" => {
                let file = path.join("ops/src/main.rs");
                let bytes = fs::read(&file).unwrap();
                fs::remove_file(&file).unwrap();
                fs::write(file, bytes).unwrap();
            }
            "membership" => fs::write(path.join("ops/src/new.rs"), b"").unwrap(),
            "directory" => {
                fs::rename(path.join("ops/src/release"), path.join("ops/src/old")).unwrap();
                fs::create_dir(path.join("ops/src/release")).unwrap();
            }
            "root" => {
                fs::rename(&path, path.with_file_name("moved")).unwrap();
                fs::create_dir(&path).unwrap();
            }
            _ => unreachable!(),
        });
        assert_eq!(result, Err(Failure::Changed), "{kind}");
    }
}

#[test]
fn unrelated_ancestor_sibling_creation_does_not_change_selected_inventory() {
    let (root, path) = source();
    let before = collect_inputs(&path).unwrap();
    let after = collect_after(&path, |_| {
        std::thread::scope(|scope| {
            scope.spawn(|| {
                fs::create_dir(root.0.join("unrelated-sibling")).unwrap();
                fs::write(root.0.join("unrelated-file"), b"unselected").unwrap();
            });
        });
    })
    .unwrap();
    assert_eq!(before, after);
}

#[test]
fn nofollow_regular_admission_and_unsupported_implicit_inputs_are_explicit() {
    for kind in [
        "file-link",
        "directory-link",
        "root-link",
        "fifo",
        "config",
        "build",
    ] {
        let (root, path) = source();
        match kind {
            "file-link" => {
                fs::remove_file(path.join("Cargo.toml")).unwrap();
                symlink(path.join("Cargo.lock"), path.join("Cargo.toml")).unwrap();
            }
            "directory-link" => {
                fs::rename(path.join("ops/src"), path.join("ops/old")).unwrap();
                symlink(path.join("ops/old"), path.join("ops/src")).unwrap();
            }
            "root-link" => {
                let link = root.0.join("link");
                symlink(&path, &link).unwrap();
                assert!(collect_inputs(&link).is_err());
                continue;
            }
            "fifo" => {
                fs::remove_file(path.join("Cargo.lock")).unwrap();
                nix::unistd::mkfifo(&path.join("Cargo.lock"), Mode::S_IRUSR).unwrap();
            }
            "config" => fs::write(path.join(".cargo/config"), b"").unwrap(),
            "build" => fs::write(path.join("ops/build.rs"), b"").unwrap(),
            _ => unreachable!(),
        }
        assert!(collect_inputs(&path).is_err(), "{kind}");
    }
}

#[test]
fn file_count_size_depth_and_directory_budgets_are_enforced_before_growth() {
    let (_root, path) = source();
    let existing = collect_inputs(&path).unwrap().files();
    for index in 0..MAX_FILES - existing {
        fs::write(path.join(format!("ops/src/extra{index}.rs")), b"").unwrap();
    }
    assert_eq!(collect_inputs(&path).unwrap().files(), MAX_FILES);
    fs::write(path.join("ops/src/overflow.rs"), b"").unwrap();
    assert_eq!(collect_inputs(&path), Err(Failure::Bounds));
    let (_root, path) = source();
    let file = fs::File::create(path.join("ops/src/main.rs")).unwrap();
    file.set_len(MAX_FILE).unwrap();
    assert!(collect_inputs(&path).is_ok());
    file.set_len(MAX_FILE + 1).unwrap();
    assert_eq!(collect_inputs(&path), Err(Failure::Bounds));
    let (_root, path) = source();
    let file = fs::File::create(path.join("Cargo.lock")).unwrap();
    file.set_len(MAX_LOCK).unwrap();
    assert!(collect_inputs(&path).is_ok());
    file.set_len(MAX_LOCK + 1).unwrap();
    assert_eq!(collect_inputs(&path), Err(Failure::Bounds));
    let (_root, path) = source();
    let nested = path.join(format!("ops/src/{}", "a/".repeat(MAX_DEPTH + 1)));
    fs::create_dir_all(nested).unwrap();
    assert!(collect_inputs(&path).is_err());
    let (_root, path) = source();
    for index in 0..MAX_DIRECTORIES {
        fs::create_dir(path.join(format!("ops/src/d{index}"))).unwrap();
    }
    assert_eq!(collect_inputs(&path), Err(Failure::Bounds));
}

#[test]
fn all_owned_descriptors_are_attempted_even_when_one_close_fails() {
    let (_root, path) = source();
    let mut tree = Tree::open(&path).unwrap();
    tree.read("Cargo.toml").unwrap();
    tree.read("ops/src/main.rs").unwrap();
    let expected = tree.files.len() + tree.directories.len();
    let mut count = 0;
    let result = tree.close_with(|file| {
        count += 1;
        close(file)?;
        if count == 1 {
            Err(Failure::Close)
        } else {
            Ok(())
        }
    });
    assert_eq!(result, Err(Failure::Close));
    assert_eq!(count, expected);
}

#[test]
fn aggregate_original_byte_budget_refuses_many_individually_valid_files() {
    let (_root, path) = source();
    for index in 0..15 {
        fs::File::create(path.join(format!("ops/src/part{index}.rs")))
            .unwrap()
            .set_len(MAX_FILE)
            .unwrap();
    }
    assert!(collect_inputs(&path).is_ok());
    fs::File::create(path.join("ops/src/overflow.rs"))
        .unwrap()
        .set_len(MAX_FILE)
        .unwrap();
    assert_eq!(collect_inputs(&path), Err(Failure::Bounds));
}
