//! Real private filesystem transactions; the production root/path selectors stay fixed.

use super::*;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

const SOURCES: &[u8] = b"# retained runner source\nTypes: deb\n\
URIs: mirror+file:/etc/apt/apt-mirrors.txt\n\
Suites: noble noble-updates noble-backports noble-security\n\
Components: main restricted universe multiverse\n\
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n";
const OS_RELEASE: &[u8] = b"NAME=Ubuntu\nID=ubuntu\nVERSION_ID=\"24.04\"\n\
VERSION_CODENAME=noble\n";

struct Fixture {
    root: PathBuf,
    owner: Owner,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "apt-mirror fixture {} {}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        for name in ["etc", "etc/apt", "etc/apt/sources.list.d", "usr", "usr/lib"] {
            fs::create_dir(root.join(name)).unwrap();
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let owner = Owner {
            uid: rustix::process::getuid().as_raw(),
            gid: rustix::process::getgid().as_raw(),
        };
        let value = Self { root, owner };
        value.write("etc/apt/apt-mirrors.txt", policy::BEFORE);
        value.write("etc/apt/sources.list.d/ubuntu.sources", SOURCES);
        value.write("usr/lib/os-release", OS_RELEASE);
        value
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        fs::write(self.root.join(name), bytes).unwrap();
        fs::set_permissions(self.root.join(name), fs::Permissions::from_mode(0o644)).unwrap();
    }

    fn mirrors(&self) -> PathBuf {
        self.root.join("etc/apt/apt-mirrors.txt")
    }

    fn staging(&self) -> PathBuf {
        self.root.join("etc/apt").join(STAGING)
    }

    fn apply(&self, hook: &mut impl FnMut(Point) -> tooling::Result<()>) -> tooling::Result<()> {
        apply_at(&self.root, self.owner, hook)
    }

    fn unchanged_guards(&self) {
        assert_eq!(
            fs::read(self.root.join("etc/apt/sources.list.d/ubuntu.sources")).unwrap(),
            SOURCES
        );
        assert_eq!(
            fs::read(self.root.join("usr/lib/os-release")).unwrap(),
            OS_RELEASE
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn replaces_only_mirror_bytes_and_idempotent_admission_does_not_rewrite() {
    let fixture = Fixture::new();
    let original = fs::metadata(fixture.mirrors()).unwrap();
    let guards = [
        "etc/apt/sources.list.d/ubuntu.sources",
        "usr/lib/os-release",
    ]
    .map(|name| fs::metadata(fixture.root.join(name)).unwrap());
    let mut closes = 0;
    fixture
        .apply(&mut |point| {
            closes += usize::from(point == Point::Close);
            Ok(())
        })
        .unwrap();
    let after = fs::metadata(fixture.mirrors()).unwrap();
    assert_eq!(fs::read(fixture.mirrors()).unwrap(), policy::AFTER);
    assert_ne!((original.dev(), original.ino()), (after.dev(), after.ino()));
    profile(&after, fixture.owner, 144).unwrap();
    assert_eq!(
        closes, 8,
        "stage, four inputs and three directory owners close"
    );
    assert!(!fixture.staging().exists());
    fixture.unchanged_guards();
    for (name, expected) in [
        "etc/apt/sources.list.d/ubuntu.sources",
        "usr/lib/os-release",
    ]
    .into_iter()
    .zip(guards)
    {
        assert!(same(
            &expected,
            &fs::metadata(fixture.root.join(name)).unwrap()
        ));
    }
    let mut points = Vec::new();
    fixture
        .apply(&mut |point| {
            points.push(point);
            Ok(())
        })
        .unwrap();
    assert!(same(&after, &fs::metadata(fixture.mirrors()).unwrap()));
    assert_eq!(points, vec![Point::Close; 6]);
}

#[test]
fn unknown_input_or_readonly_guard_refuses_before_any_write() {
    for (name, bytes) in [
        ("etc/apt/apt-mirrors.txt", b"".as_slice()),
        (
            "etc/apt/apt-mirrors.txt",
            b"https://private.invalid/\tpriority:1\n",
        ),
        ("etc/apt/apt-mirrors.txt", &[b'x'; 145]),
        (
            "etc/apt/sources.list.d/ubuntu.sources",
            b"URIs: https://private.invalid/\n",
        ),
        (
            "usr/lib/os-release",
            b"ID=ubuntu\nVERSION_ID=22.04\nVERSION_CODENAME=jammy\n",
        ),
    ] {
        let fixture = Fixture::new();
        fixture.write(name, bytes);
        let mut points = Vec::new();
        assert!(
            fixture
                .apply(&mut |point| {
                    points.push(point);
                    Ok(())
                })
                .is_err()
        );
        assert!(points.iter().all(|point| *point == Point::Close));
        assert_eq!(fs::read(fixture.root.join(name)).unwrap(), bytes);
        assert!(!fixture.staging().exists());
    }
}

#[test]
fn regular_inputs_require_exact_mode_single_link_and_original_owner() {
    for name in [
        "etc/apt/apt-mirrors.txt",
        "etc/apt/sources.list.d/ubuntu.sources",
        "usr/lib/os-release",
    ] {
        for mode in [0o600, 0o664, 0o755, 0o4644] {
            let fixture = Fixture::new();
            fs::set_permissions(fixture.root.join(name), fs::Permissions::from_mode(mode)).unwrap();
            assert!(fixture.apply(&mut |_| Ok(())).is_err(), "{name}: {mode:o}");
            assert_eq!(fs::read(fixture.mirrors()).unwrap(), policy::BEFORE);
            assert!(!fixture.staging().exists());
        }
        let fixture = Fixture::new();
        fs::hard_link(fixture.root.join(name), fixture.root.join("second-link")).unwrap();
        assert!(fixture.apply(&mut |_| Ok(())).is_err(), "hard link: {name}");
        assert_eq!(fs::metadata(fixture.root.join(name)).unwrap().nlink(), 2);
        let fixture = Fixture::new();
        let original = fixture.root.join("original-file");
        fs::rename(fixture.root.join(name), &original).unwrap();
        symlink(&original, fixture.root.join(name)).unwrap();
        assert!(fixture.apply(&mut |_| Ok(())).is_err(), "symlink: {name}");
        assert!(
            fs::symlink_metadata(fixture.root.join(name))
                .unwrap()
                .is_symlink()
        );
    }
    let fixture = Fixture::new();
    let metadata = fs::metadata(fixture.mirrors()).unwrap();
    let wrong_owner = Owner {
        uid: fixture.owner.uid.checked_add(1).unwrap(),
        gid: fixture.owner.gid,
    };
    assert!(profile(&metadata, wrong_owner, 144).is_err());
}

#[test]
fn every_fixed_directory_requires_nofollow_owned_nonwritable_binding() {
    for name in ["etc", "etc/apt", "etc/apt/sources.list.d", "usr", "usr/lib"] {
        let fixture = Fixture::new();
        fs::set_permissions(fixture.root.join(name), fs::Permissions::from_mode(0o777)).unwrap();
        assert!(fixture.apply(&mut |_| Ok(())).is_err(), "mode: {name}");
        assert_eq!(fs::read(fixture.mirrors()).unwrap(), policy::BEFORE);
        let fixture = Fixture::new();
        let original = fixture.root.join("original-directory");
        fs::rename(fixture.root.join(name), &original).unwrap();
        symlink(&original, fixture.root.join(name)).unwrap();
        assert!(fixture.apply(&mut |_| Ok(())).is_err(), "symlink: {name}");
        assert!(
            fs::symlink_metadata(fixture.root.join(name))
                .unwrap()
                .is_symlink()
        );
    }
    let fixture = Fixture::new();
    fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(fixture.apply(&mut |_| Ok(())).is_err());
    assert_eq!(fs::read(fixture.mirrors()).unwrap(), policy::BEFORE);
}

#[test]
fn existing_staging_entries_are_never_overwritten_or_removed() {
    for kind in ["file", "symlink", "hardlink"] {
        let fixture = Fixture::new();
        let foreign = fixture.root.join("foreign");
        fs::write(&foreign, b"foreign sentinel").unwrap();
        match kind {
            "file" => fs::write(fixture.staging(), b"foreign sentinel").unwrap(),
            "symlink" => symlink(&foreign, fixture.staging()).unwrap(),
            "hardlink" => fs::hard_link(&foreign, fixture.staging()).unwrap(),
            _ => unreachable!(),
        }
        let before = fs::symlink_metadata(fixture.staging()).unwrap();
        assert!(fixture.apply(&mut |_| Ok(())).is_err());
        assert!(same(
            &before,
            &fs::symlink_metadata(fixture.staging()).unwrap()
        ));
        assert_eq!(fs::read(fixture.staging()).unwrap(), b"foreign sentinel");
        assert_eq!(fs::read(fixture.mirrors()).unwrap(), policy::BEFORE);
    }
}

#[test]
fn original_or_staged_replacement_before_publication_cannot_be_overwritten() {
    for target in ["original-data", "original-inode", "staging-inode"] {
        let fixture = Fixture::new();
        let mut changed = false;
        let mut closes = 0;
        let result = fixture.apply(&mut |point| {
            closes += usize::from(point == Point::Close);
            if point == Point::BeforeRename && !changed {
                changed = true;
                match target {
                    "original-data" => fixture.write("etc/apt/apt-mirrors.txt", policy::AFTER),
                    "original-inode" => {
                        fs::rename(fixture.mirrors(), fixture.root.join("retired-original"))
                            .unwrap();
                        fixture.write("etc/apt/apt-mirrors.txt", policy::AFTER);
                    }
                    "staging-inode" => {
                        fs::rename(fixture.staging(), fixture.root.join("retired-stage")).unwrap();
                        fs::write(fixture.staging(), b"foreign staging sentinel").unwrap();
                    }
                    _ => unreachable!(),
                }
            }
            Ok(())
        });
        assert!(result.is_err(), "{target}");
        assert!(changed);
        assert_eq!(closes, 7, "all admitted owners still finish: {target}");
        if target == "staging-inode" {
            assert_eq!(
                fs::read(fixture.staging()).unwrap(),
                b"foreign staging sentinel"
            );
            assert_eq!(fs::read(fixture.mirrors()).unwrap(), policy::BEFORE);
        } else {
            assert_eq!(fs::read(fixture.mirrors()).unwrap(), policy::AFTER);
            assert!(!fixture.staging().exists());
        }
    }
}

#[test]
fn actual_rename_refusal_preserves_original_and_never_removes_a_different_name() {
    let fixture = Fixture::new();
    let retired = fixture.root.join("moved-owned-stage");
    let mut moved = false;
    let mut closes = 0;
    assert!(
        fixture
            .apply(&mut |point| {
                closes += usize::from(point == Point::Close);
                if point == Point::Rename {
                    fs::rename(fixture.staging(), &retired).unwrap();
                    moved = true;
                }
                Ok(())
            })
            .is_err()
    );
    assert!(moved);
    assert_eq!(closes, 7);
    assert_eq!(fs::read(fixture.mirrors()).unwrap(), policy::BEFORE);
    assert_eq!(fs::read(retired).unwrap(), policy::AFTER);
    assert!(!fixture.staging().exists());
}

#[test]
fn write_sync_readback_rename_and_late_faults_preserve_first_failure_and_finish_owners() {
    for selected in [
        Point::Write,
        Point::FileSync,
        Point::Readback,
        Point::BeforeRename,
        Point::Rename,
        Point::DirectorySync,
        Point::Published,
    ] {
        let fixture = Fixture::new();
        let mut failed = false;
        let mut closes = 0;
        let error = fixture
            .apply(&mut |point| {
                closes += usize::from(point == Point::Close);
                if point == selected && !failed {
                    failed = true;
                    return Err(tooling::Failure::new(1, "first injected failure"));
                }
                if point == Point::Close && failed {
                    return Err(tooling::Failure::new(1, "later close uncertainty"));
                }
                Ok(())
            })
            .unwrap_err();
        assert!(failed, "{selected:?}");
        assert_eq!(error.message, "first injected failure");
        assert_eq!(closes, if selected == Point::Published { 8 } else { 7 });
        assert_eq!(
            fs::read(fixture.mirrors()).unwrap(),
            if matches!(selected, Point::DirectorySync | Point::Published) {
                policy::AFTER
            } else {
                policy::BEFORE
            }
        );
        assert!(!fixture.staging().exists());
        fixture.unchanged_guards();
    }
}

#[test]
fn each_owned_close_uncertainty_refuses_without_skipping_remaining_closes() {
    for selected in 1..=8 {
        let fixture = Fixture::new();
        let mut closes = 0;
        let result = fixture.apply(&mut |point| {
            if point == Point::Close {
                closes += 1;
                if closes == selected {
                    return Err(tooling::Failure::new(1, "checked close uncertainty"));
                }
            }
            Ok(())
        });
        assert!(result.is_err(), "close {selected}");
        assert_eq!(closes, if selected == 1 { 7 } else { 8 });
        assert!(!fixture.staging().exists());
        assert_eq!(
            fs::read(fixture.mirrors()).unwrap(),
            if selected == 1 {
                policy::BEFORE
            } else {
                policy::AFTER
            }
        );
    }
}

#[test]
fn readback_tampering_or_late_readonly_guard_change_never_reports_success() {
    for selected in [Point::Readback, Point::Published] {
        let fixture = Fixture::new();
        let mut changed = false;
        assert!(
            fixture
                .apply(&mut |point| {
                    if point == selected && !changed {
                        changed = true;
                        if point == Point::Readback {
                            fs::write(fixture.staging(), [b'x'; 144]).unwrap();
                        } else {
                            fixture.write("usr/lib/os-release", b"changed readonly sentinel\n");
                        }
                    }
                    Ok(())
                })
                .is_err()
        );
        assert!(changed);
        assert!(!fixture.staging().exists());
        assert_eq!(
            fs::read(fixture.mirrors()).unwrap(),
            if selected == Point::Published {
                policy::AFTER
            } else {
                policy::BEFORE
            }
        );
    }
}

#[test]
fn published_target_mutations_seen_during_finalization_refuse_without_rollback() {
    for (point, replace_inode) in [
        (Point::Published, false),
        (Point::Published, true),
        (Point::Close, false),
        (Point::Close, true),
    ] {
        let fixture = Fixture::new();
        let mut published = false;
        let mut changed = false;
        let mut closes = 0;
        let result = fixture.apply(&mut |actual| {
            closes += usize::from(actual == Point::Close);
            if actual == Point::Published {
                published = true;
            }
            if published && actual == point && !changed {
                changed = true;
                if replace_inode {
                    fs::rename(fixture.mirrors(), fixture.root.join("displaced-preferred"))
                        .unwrap();
                }
                fixture.write("etc/apt/apt-mirrors.txt", b"late foreign sentinel\n");
            }
            Ok(())
        });
        assert!(changed);
        assert!(result.is_err(), "{point:?}, replace={replace_inode}");
        assert_eq!(closes, 8);
        assert_eq!(
            fs::read(fixture.mirrors()).unwrap(),
            b"late foreign sentinel\n"
        );
        assert!(!fixture.staging().exists());
        fixture.unchanged_guards();
    }
}
