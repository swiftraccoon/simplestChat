//! Original namespace and writer witnesses, including refused special files.

use super::super::namespace::Budget;
use super::{files::Tree, fixture};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

#[test]
fn private_tree_requires_original_content_mode_namespace_and_no_adopted_replacements() {
    for case in ["bytes", "inode", "special-mode", "symlink", "extra", "fifo"] {
        let fixture = fixture::Fixture::new();
        let budget = Budget::default();
        let mut tree = Tree::create(&fixture.root.join("tree"), &budget).unwrap();
        tree.write("nested/original", b"original", false).unwrap();
        let leaf = tree.run.path.join("nested/original");
        match case {
            "bytes" => fs::write(&leaf, b"tampered").unwrap(),
            "inode" => {
                fs::rename(&leaf, fixture.root.join("held-original")).unwrap();
                fs::write(&leaf, b"original").unwrap();
            }
            "special-mode" => {
                fs::set_permissions(&leaf, fs::Permissions::from_mode(0o4600)).unwrap()
            }
            "symlink" => {
                fs::remove_file(&leaf).unwrap();
                symlink("PRIVATE-missing", &leaf).unwrap();
            }
            "extra" => fs::write(tree.run.path.join("PRIVATE-extra"), b"unowned").unwrap(),
            "fifo" => {
                fs::remove_file(&leaf).unwrap();
                nix::unistd::mkfifo(&leaf, nix::sys::stat::Mode::from_bits_truncate(0o600))
                    .unwrap();
            }
            _ => unreachable!(),
        }
        fixture::expect_error(tree.check(), 1);
        fixture::expect_error(tree.finish(), 1);
        assert_eq!(budget.count(), 0);
        let (attempted, fallback) = budget.close_counts();
        assert!(attempted > 0);
        assert_eq!(fallback, 0);
    }
}

#[test]
fn failed_writer_closes_all_owners_and_never_adopts_partial_bytes() {
    let fixture = fixture::Fixture::new();
    let budget = Budget::default();
    let mut tree = Tree::create(&fixture.root.join("tree"), &budget).unwrap();
    let mut checks = 0;
    let result = tree.stream(
        "nested/partial",
        131072,
        false,
        &mut std::io::repeat(7),
        &mut || {
            checks += 1;
            if checks == 2 {
                Err(super::failure(143))
            } else {
                Ok(())
            }
        },
    );
    fixture::expect_error(result, 143);
    assert!(!tree.records.contains_key("nested/partial"));
    fixture::expect_error(tree.finish(), 1);
    assert_eq!(budget.count(), 0);
    let (attempted, fallback) = budget.close_counts();
    assert!(attempted > 0);
    assert_eq!(fallback, 0);
}

#[test]
fn only_fixed_independently_governed_registry_can_be_delegated() {
    let fixture = fixture::Fixture::new();
    let budget = Budget::default();
    let mut tree = Tree::create(&fixture.root.join("tree"), &budget).unwrap();
    tree.write("toolchain/bin/cargo", b"inert", true).unwrap();
    fs::create_dir(tree.run.path.join("registry")).unwrap();
    fs::set_permissions(
        tree.run.path.join("registry"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    fs::write(
        tree.run.path.join("registry/third-party-file"),
        b"governed by registry frame",
    )
    .unwrap();
    assert!(tree.check().is_err());
    tree.registry_created().unwrap();
    tree.check().unwrap();
    assert!(tree.registry_created().is_err());
    fs::rename(
        tree.run.path.join("registry"),
        fixture.root.join("original-registry"),
    )
    .unwrap();
    fs::create_dir(tree.run.path.join("registry")).unwrap();
    fixture::expect_error(tree.finish(), 1);
    assert_eq!(budget.count(), 0);
}
