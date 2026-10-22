//! Owned runner files and deterministic replacement/close seams, no queries.
use super::*;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};
struct Fixture(PathBuf);
impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "media env λ {} {}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("environment"), bytes).unwrap();
        fs::set_permissions(root.join("environment"), fs::Permissions::from_mode(0o600)).unwrap();
        Self(root)
    }
    fn path(&self) -> PathBuf {
        self.0.join("environment")
    }
    fn environment(&self) -> Environment {
        [("GITHUB_ENV".into(), self.path().into_os_string())].into()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn fixed_line_appends_preserving_existing_utf8_bytes_and_explicit_close() {
    for before in [b"".as_slice(), "OTHER=private λ with spaces\n".as_bytes()] {
        let fixture = Fixture::new(before);
        let mut target = Target::open(&fixture.environment()).unwrap();
        target.append(Ipv4Addr::new(192, 0, 2, 44)).unwrap();
        target.check().unwrap();
        target.finish().unwrap();
        let mut expected = before.to_vec();
        expected.extend_from_slice(b"TEST_ANNOUNCE_IP=192.0.2.44\n");
        assert_eq!(fs::read(fixture.path()).unwrap(), expected);
    }
}
#[test]
fn malformed_missing_unsafe_or_full_files_refuse_without_append_or_truncate() {
    for before in [
        b"missing newline".to_vec(),
        b"NUL=\0\n".to_vec(),
        b"CR=1\r\n".to_vec(),
        vec![255, 10],
        vec![b'x'; LIMIT as usize + 1],
    ] {
        let fixture = Fixture::new(&before);
        assert!(Target::open(&fixture.environment()).is_err());
        assert_eq!(fs::read(fixture.path()).unwrap(), before);
    }
    let fixture = Fixture::new(b"KEPT=1\n");
    assert!(Target::open(&Environment::new()).is_err());
    fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o666)).unwrap();
    assert!(Target::open(&fixture.environment()).is_err());
    fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o600)).unwrap();
    fs::hard_link(fixture.path(), fixture.0.join("linked")).unwrap();
    assert!(Target::open(&fixture.environment()).is_err());
    fs::remove_file(fixture.0.join("linked")).unwrap();
    fs::rename(fixture.path(), fixture.0.join("original")).unwrap();
    symlink(fixture.0.join("original"), fixture.path()).unwrap();
    assert!(Target::open(&fixture.environment()).is_err());
    fs::remove_file(fixture.path()).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(fixture.path())
            .status()
            .unwrap()
            .success()
    );
    assert!(Target::open(&fixture.environment()).is_err());
}
#[test]
fn record_size_is_admitted_before_the_first_append() {
    let mut bytes = vec![b'x'; LIMIT as usize];
    *bytes.last_mut().unwrap() = b'\n';
    let fixture = Fixture::new(&bytes);
    let mut target = Target::open(&fixture.environment()).unwrap();
    assert!(target.append(Ipv4Addr::new(192, 0, 2, 44)).is_err());
    target.finish().unwrap();
    assert_eq!(fs::read(fixture.path()).unwrap(), bytes);
}
#[test]
fn replacement_before_admission_does_not_modify_either_file() {
    let fixture = Fixture::new(b"KEPT=1\n");
    let mut target = Target::open(&fixture.environment()).unwrap();
    fs::rename(fixture.path(), fixture.0.join("retired")).unwrap();
    fs::write(fixture.path(), b"FOREIGN=1\n").unwrap();
    assert!(target.append(Ipv4Addr::new(192, 0, 2, 44)).is_err());
    assert!(target.finish().is_err());
    assert_eq!(fs::read(fixture.path()).unwrap(), b"FOREIGN=1\n");
    assert_eq!(fs::read(fixture.0.join("retired")).unwrap(), b"KEPT=1\n");
}
#[test]
fn replacement_during_append_is_bound_to_original_fd_and_cannot_be_success() {
    let fixture = Fixture::new(b"KEPT=1\n");
    let mut target = Target::open(&fixture.environment()).unwrap();
    let result = target.append_with(Ipv4Addr::new(192, 0, 2, 44), || {
        fs::rename(fixture.path(), fixture.0.join("retired")).unwrap();
        fs::write(fixture.path(), b"FOREIGN=1\n").unwrap();
    });
    assert!(result.is_err());
    assert!(target.finish().is_err());
    assert_eq!(fs::read(fixture.path()).unwrap(), b"FOREIGN=1\n");
    assert_eq!(
        fs::read(fixture.0.join("retired")).unwrap(),
        b"KEPT=1\nTEST_ANNOUNCE_IP=192.0.2.44\n"
    );
}
#[test]
fn observed_close_failure_refuses_success_after_already_visible_append() {
    let fixture = Fixture::new(b"");
    let mut target = Target::open(&fixture.environment()).unwrap();
    target.append(Ipv4Addr::new(192, 0, 2, 44)).unwrap();
    assert!(
        target
            .finish_with(|file| {
                close(file)?;
                Err(fail())
            })
            .is_err()
    );
    assert_eq!(
        fs::read(fixture.path()).unwrap(),
        b"TEST_ANNOUNCE_IP=192.0.2.44\n"
    );
}
