//! Owned filesystem fixtures for literal admission and private exclusive copies.

use super::*;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture {
    directory: PathBuf,
    runner: PathBuf,
    evidence: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "ci evidence {} {} with spaces",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let runner = directory.join("runner");
        let evidence = directory.join("evidence");
        for path in [&runner, &evidence] {
            fs::create_dir(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self {
            directory,
            runner,
            evidence,
        }
    }
    fn env(&self) -> Environment {
        [
            ("RUNNER_TEMP".into(), self.runner.clone().into_os_string()),
            (
                "LOCAL_CI_EVIDENCE".into(),
                self.evidence.clone().into_os_string(),
            ),
        ]
        .into()
    }
    fn source(&self, kind: Kind) -> PathBuf {
        let source = self.runner.join(kind.source());
        fs::create_dir(&source).unwrap();
        source
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.directory).unwrap();
    }
}

fn inventory(path: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn collect(root: &Path, path: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        assert_eq!(path.metadata().unwrap().mode() & 0o777, 0o700);
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect(root, &path, files);
            } else {
                assert_eq!(path.metadata().unwrap().mode() & 0o777, 0o600);
                files.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    collect(path, path, &mut files);
    files
}

#[test]
fn all_fixed_plans_copy_only_the_original_literal_allowlists_with_private_modes() {
    for kind in [
        Kind::ImageDatabase,
        Kind::ReleaseContainer,
        Kind::ReleaseChecks,
    ] {
        let fixture = Fixture::new();
        let source = fixture.source(kind);
        let admitted: &[&str] = match kind {
            Kind::ImageDatabase => &[
                "database-preparation.json",
                "database-status.json",
                "scanner-result-01.json",
                "scanner-result-$(literal).json",
                "scanner-result-empty.json",
            ],
            Kind::ReleaseContainer => &[
                "report.json",
                "private/command-049.json",
                "private/command-049.stdout",
                "private/command-049.stderr",
                "private/command-054.json",
                "private/command-054.stdout",
                "private/command-054.stderr",
                "private/command-$(literal).stderr",
                "private/command-empty.stderr",
            ],
            Kind::ReleaseChecks => &[
                "summary.json",
                "release-fixture.stdout",
                "release-fixture.stderr",
                "scanner-tools.stdout",
                "scanner-tools.stderr",
            ],
        };
        fs::create_dir(source.join("private")).unwrap();
        let mut expected = BTreeMap::new();
        for name in admitted {
            let body = if name.contains("empty") {
                Vec::new()
            } else {
                format!("private {name}\n").into_bytes()
            };
            fs::write(source.join(name), &body).unwrap();
            expected.insert(PathBuf::from(name), body);
        }
        for name in [
            "ca.key",
            "fixture-image.tar",
            "app.env",
            "private/ca.key",
            "private/fixture-image.tar",
            "private/app.env",
            "command-001.stderr",
        ] {
            fs::write(source.join(name), "not retained").unwrap();
        }
        symlink(
            source.join("private/ca.key"),
            source.join("private/command-999.stderr"),
        )
        .unwrap();
        symlink(
            source.join("ca.key"),
            source.join("scanner-result-link.json"),
        )
        .unwrap();
        fs::create_dir(source.join("scanner-result-directory.json")).unwrap();
        assert!(
            std::process::Command::new("/usr/bin/mkfifo")
                .arg(source.join("scanner-result-fifo.json"))
                .status()
                .unwrap()
                .success()
        );
        project(kind, &fixture.env()).unwrap();
        assert_eq!(
            inventory(&fixture.evidence.join(kind.destination())),
            expected
        );
        for (name, body) in expected {
            assert_eq!(fs::read(source.join(name)).unwrap(), body);
        }
        assert!(!fixture.directory.join("literal").exists());
    }
}

#[test]
fn missing_or_unsafe_optional_sources_do_not_require_a_destination_or_create_output() {
    let fixture = Fixture::new();
    let mut environment = fixture.env();
    environment.remove(OsStr::new("LOCAL_CI_EVIDENCE"));
    project(Kind::ReleaseContainer, &environment).unwrap();
    let source = fixture.runner.join("release-container");
    fs::write(&source, "not a directory").unwrap();
    project(Kind::ReleaseContainer, &environment).unwrap();
    fs::remove_file(&source).unwrap();
    symlink(&fixture.evidence, &source).unwrap();
    project(Kind::ReleaseContainer, &environment).unwrap();
    assert_eq!(fs::read_dir(&fixture.evidence).unwrap().count(), 0);
}

#[test]
fn empty_and_failed_fixture_sources_keep_optional_report_and_private_directory_semantics() {
    for report in [false, true] {
        let fixture = Fixture::new();
        let source = fixture.source(Kind::ReleaseContainer);
        if report {
            fs::write(source.join("report.json"), b"{\"passed\":false}").unwrap();
        }
        symlink(&fixture.directory, source.join("private")).unwrap();
        project(Kind::ReleaseContainer, &fixture.env()).unwrap();
        let destination = fixture.evidence.join("release-container");
        assert!(destination.join("private").is_dir());
        assert_eq!(inventory(&destination).len(), usize::from(report));
    }
}

#[test]
fn existing_output_and_nonprivate_or_symlink_roots_fail_without_overwriting_evidence() {
    let fixture = Fixture::new();
    fixture.source(Kind::ReleaseChecks);
    let destination = fixture.evidence.join("release-checks");
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("marker"), "original").unwrap();
    assert!(project(Kind::ReleaseChecks, &fixture.env()).is_err());
    assert_eq!(
        fs::read_to_string(destination.join("marker")).unwrap(),
        "original"
    );
    fs::remove_dir_all(&destination).unwrap();
    symlink(&fixture.directory, &destination).unwrap();
    assert!(project(Kind::ReleaseChecks, &fixture.env()).is_err());
    fs::remove_file(&destination).unwrap();
    fs::set_permissions(&fixture.evidence, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(project(Kind::ReleaseChecks, &fixture.env()).is_err());
    let mut environment = fixture.env();
    let alias = fixture.directory.join("alias");
    symlink(&fixture.evidence, &alias).unwrap();
    environment.insert("LOCAL_CI_EVIDENCE".into(), alias.into_os_string());
    assert!(project(Kind::ReleaseChecks, &environment).is_err());
}

#[test]
fn descriptor_relative_copies_cannot_follow_replaced_source_or_destination_paths() {
    let fixture = Fixture::new();
    let source_path = fixture.source(Kind::ReleaseChecks);
    fs::write(source_path.join("summary.json"), "owned source").unwrap();
    let runner = root(&fixture.env(), "RUNNER_TEMP").unwrap();
    let source = optional_directory(&runner, "release-checks")
        .unwrap()
        .unwrap();
    fs::rename(&source_path, fixture.runner.join("old-source")).unwrap();
    symlink(&fixture.directory, &source_path).unwrap();
    let evidence = root(&fixture.env(), "LOCAL_CI_EVIDENCE").unwrap();
    let destination = new_directory(&evidence, "release-checks").unwrap();
    fs::rename(
        fixture.evidence.join("release-checks"),
        fixture.evidence.join("old-output"),
    )
    .unwrap();
    symlink(&fixture.directory, fixture.evidence.join("release-checks")).unwrap();
    copy(
        &source,
        &destination,
        OsStr::new("summary.json"),
        &mut Budget::default(),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(fixture.evidence.join("old-output/summary.json")).unwrap(),
        "owned source"
    );
    assert!(!fixture.directory.join("summary.json").exists());
    assert!(
        private_directory_is_at(&evidence, OsStr::new("release-checks"), &destination).is_err()
    );
}

#[test]
fn file_count_byte_and_inventory_budgets_fail_closed() {
    let fixture = Fixture::new();
    let source = fixture.source(Kind::ImageDatabase);
    File::create(source.join("database-preparation.json"))
        .unwrap()
        .set_len(MAX_FILE + 1)
        .unwrap();
    assert!(project(Kind::ImageDatabase, &fixture.env()).is_err());
    assert!(
        !fixture
            .evidence
            .join("image-database/database-preparation.json")
            .exists()
    );
    let mut count = Budget {
        files: MAX_FILES - 1,
        bytes: 0,
    };
    count.admit(0).unwrap();
    assert!(count.admit(0).is_err());
    let mut bytes = Budget {
        files: 0,
        bytes: MAX_TOTAL - MAX_FILE,
    };
    bytes.admit(MAX_FILE).unwrap();
    assert!(bytes.admit(1).is_err());
    for index in 0..=MAX_ENTRIES {
        fs::write(source.join(format!("unrelated-{index}")), "").unwrap();
    }
    let runner = root(&fixture.env(), "RUNNER_TEMP").unwrap();
    let source = optional_directory(&runner, "image-database-preparation")
        .unwrap()
        .unwrap();
    assert!(names(&source, b"scanner-result-", b".json").is_err());
}
