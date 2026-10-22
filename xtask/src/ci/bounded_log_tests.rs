//! Inert private-file and pipe tests for the shared bounded combined log.

use super::*;
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::AtomicU64,
    time::Instant,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "bounded-log-fixture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
    fn file(&self) -> File {
        OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(self.0.join("output.log"))
            .unwrap()
    }
    fn bytes(&self) -> Vec<u8> {
        fs::read(self.0.join("output.log")).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[tokio::test]
async fn combined_writers_preserve_bytes_digest_and_original_output_descriptor() {
    let fixture = Fixture::new();
    let log = Log::start(fixture.file(), 64).unwrap();
    // The core has only the admitted descriptor; it must never reopen a path.
    fs::rename(fixture.0.join("output.log"), fixture.0.join("original.log")).unwrap();
    fs::write(fixture.0.join("output.log"), b"replacement").unwrap();
    let mut stdout = log.writer().unwrap();
    let mut stderr = log.writer().unwrap();
    stdout.write_all(b"stdout\n").unwrap();
    stderr.write_all(b"stderr\n").unwrap();
    close(stdout).unwrap();
    close(stderr).unwrap();
    let done = log.finish().await.unwrap();
    assert!(done.complete);
    assert_eq!(done.bytes, 14);
    assert_eq!(done.metadata.len(), 14);
    assert_eq!(
        done.sha256,
        "f7047c7da981acbadc372dd699784ec46eda4ffb15c03fe25ac25bc595eed04b"
    );
    assert_eq!(fixture.bytes(), b"replacement");
    assert_eq!(
        fs::read(fixture.0.join("original.log")).unwrap(),
        b"stdout\nstderr\n"
    );
}

#[tokio::test]
async fn exact_cap_completes_but_one_excess_byte_cannot_reach_disk_or_pass() {
    for payload in [b"01234567".as_slice(), b"012345678"] {
        let fixture = Fixture::new();
        let log = Log::start(fixture.file(), 8).unwrap();
        let mut writer = log.writer().unwrap();
        writer.write_all(payload).unwrap();
        close(writer).unwrap();
        let done = log.finish().await.unwrap();
        assert_eq!(done.complete, payload.len() == 8);
        assert_eq!(done.bytes, 8);
        assert_eq!(fixture.bytes(), b"01234567");
    }
}

#[tokio::test]
async fn a_retained_foreign_writer_has_bounded_incomplete_eof_without_a_reader_thread() {
    let fixture = Fixture::new();
    let log = Log::start(fixture.file(), 8).unwrap();
    let writer = log.writer().unwrap();
    let start = Instant::now();
    let done = log.finish().await.unwrap();
    assert!(!done.complete);
    assert!(start.elapsed() < Duration::from_secs(4));
    close(writer).unwrap();
}

#[tokio::test]
async fn invalid_output_admission_closes_the_received_descriptor() {
    use std::os::fd::AsRawFd;
    for nonempty in [false, true] {
        let fixture = Fixture::new();
        let mut output = fixture.file();
        if nonempty {
            output.write_all(b"private-content").unwrap();
        } else {
            output
                .set_permissions(fs::Permissions::from_mode(0o644))
                .unwrap();
        }
        let fd = output.as_raw_fd();
        assert!(Log::start(output, 8).is_err());
        assert!(!std::path::Path::new(&format!("/dev/fd/{fd}")).exists());
    }
}

#[tokio::test]
async fn an_output_write_failure_is_private_and_cannot_be_complete() {
    let fixture = Fixture::new();
    close(fixture.file()).unwrap();
    let output = File::open(fixture.0.join("output.log")).unwrap();
    let log = Log::start(output, 64).unwrap();
    let mut writer = log.writer().unwrap();
    writer.write_all(b"private-content").unwrap();
    close(writer).unwrap();
    let error = log.finish().await.err().unwrap();
    assert!(!error.message.contains("private-content"));
    assert!(fixture.bytes().is_empty());
}
