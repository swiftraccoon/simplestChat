//! Real owned files exercise admission, mutation, no-clobber and close failures.

use super::*;
use crate::ci::container_smoke::options::Mode;
use crate::tooling::Environment;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt, symlink},
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture {
    root: PathBuf,
    context: Context,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "container files {} {}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        fs::create_dir_all(root.join("web/public")).unwrap();
        fs::create_dir(root.join("migrations")).unwrap();
        for asset in Asset::ALL {
            fs::write(root.join("web/public").join(asset.name()), asset.name()).unwrap();
        }
        fs::write(root.join("migrations/1.sql"), "select 1").unwrap();
        let environment = Environment::from([(
            "CONTAINER_TEST_ARTIFACTS".into(),
            root.join("artifacts").into_os_string(),
        )]);
        let context = Context::new(&root, environment, Mode::Local, "Darwin").unwrap();
        Self { root, context }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn sources_are_bounded_snapshots_but_migration_count_does_not_allocate_sql_contents() {
    let fixture = Fixture::new();
    File::options()
        .write(true)
        .open(fixture.root.join("migrations/1.sql"))
        .unwrap()
        .set_len(MAX_ASSET + 1)
        .unwrap();
    let inputs = Inputs::read(&fixture.root).unwrap();
    assert_eq!(inputs.migrations(), 1);
    assert_eq!(inputs.asset(Asset::Manifest), b"manifest.webmanifest");
    inputs.finish().unwrap();
    File::options()
        .write(true)
        .open(fixture.root.join("web/public/sw.js"))
        .unwrap()
        .set_len(MAX_ASSET + 1)
        .unwrap();
    assert!(Inputs::read(&fixture.root).is_err());
}

#[test]
fn final_links_special_nodes_ancestor_links_and_source_changes_are_refused() {
    for case in ["symlink", "directory", "ancestor", "mutation", "inventory"] {
        let fixture = Fixture::new();
        match case {
            "symlink" => {
                fs::remove_file(fixture.root.join("web/public/sw.js")).unwrap();
                symlink(
                    fixture.root.join("migrations/1.sql"),
                    fixture.root.join("web/public/sw.js"),
                )
                .unwrap();
            }
            "directory" => {
                fs::remove_file(fixture.root.join("web/public/sw.js")).unwrap();
                fs::create_dir(fixture.root.join("web/public/sw.js")).unwrap();
            }
            "ancestor" => {
                fs::rename(
                    fixture.root.join("web/public"),
                    fixture.root.join("actual-public"),
                )
                .unwrap();
                symlink(
                    fixture.root.join("actual-public"),
                    fixture.root.join("web/public"),
                )
                .unwrap();
            }
            _ => {}
        }
        if matches!(case, "mutation" | "inventory") {
            let inputs = Inputs::read(&fixture.root).unwrap();
            if case == "mutation" {
                fs::write(fixture.root.join("web/public/sw.js"), "changed").unwrap();
            } else {
                fs::write(fixture.root.join("migrations/2.sql"), "select 2").unwrap();
            }
            assert!(inputs.finish().is_err(), "{case}");
        } else {
            assert!(Inputs::read(&fixture.root).is_err(), "{case}");
        }
    }
}

#[test]
fn artifacts_are_private_exclusive_and_never_write_through_substituted_namespace() {
    let fixture = Fixture::new();
    let artifacts = Artifacts::new(&fixture.context).unwrap();
    assert_eq!(fs::metadata(&artifacts.path).unwrap().mode() & 0o777, 0o700);
    artifacts.write("rooms.json", b"[]").unwrap();
    assert!(artifacts.write("rooms.json", b"changed").is_err());
    assert!(artifacts.write("../outside", b"changed").is_err());
    assert_eq!(
        fs::metadata(artifacts.path.join("rooms.json"))
            .unwrap()
            .mode()
            & 0o777,
        0o600
    );
    fs::rename(&artifacts.path, fixture.root.join("original-artifacts")).unwrap();
    fs::create_dir(fixture.root.join("unrelated")).unwrap();
    symlink(fixture.root.join("unrelated"), &artifacts.path).unwrap();
    assert!(artifacts.write("index.html", b"changed").is_err());
    assert!(!fixture.root.join("unrelated/index.html").exists());
    assert!(artifacts.finish().is_err());
}

#[test]
fn existing_output_is_never_adopted_even_when_empty_private_or_a_final_symlink() {
    for case in ["empty", "nonempty", "public", "symlink"] {
        let fixture = Fixture::new();
        let path = fixture.root.join("artifacts");
        if case == "symlink" {
            symlink(fixture.root.join("web/public"), &path).unwrap();
        } else {
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            if case == "nonempty" {
                fs::write(path.join("keep"), "unchanged").unwrap();
            } else if case == "public" {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        assert!(Artifacts::new(&fixture.context).is_err(), "{case}");
        if case == "nonempty" {
            assert_eq!(fs::read_to_string(path.join("keep")).unwrap(), "unchanged");
        }
    }
}

#[test]
fn selected_temp_parent_alias_is_frozen_physically_and_all_directory_closes_are_attempted() {
    let fixture = Fixture::new();
    symlink(&fixture.root, fixture.root.join("alias")).unwrap();
    let mut context =
        Context::new(&fixture.root, Environment::new(), Mode::Local, "Darwin").unwrap();
    context.temporary = fixture.root.join("alias");
    let artifacts = Artifacts::new(&context).unwrap();
    assert_eq!(artifacts.path.parent(), Some(fixture.root.as_path()));
    artifacts.finish().unwrap();
    let directory = Directory::open(&fixture.root).unwrap();
    let expected = directory.files.len();
    let mut closed = 0;
    assert!(
        directory
            .finish_with(|file| {
                closed += 1;
                close(file)?;
                if closed == 1 { Err(failure()) } else { Ok(()) }
            })
            .is_err()
    );
    assert_eq!(closed, expected);
}
