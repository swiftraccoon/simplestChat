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
async fn live_progress_binds_pipe_bytes_even_when_the_output_file_is_modified() {
    use std::os::unix::fs::FileExt;

    let fixture = Fixture::new();
    let mut log = Log::start(fixture.file(), 64).unwrap();
    let mut producer = log.writer().unwrap();
    let outside = OpenOptions::new()
        .write(true)
        .open(fixture.0.join("output.log"))
        .unwrap();
    outside.write_all_at(b"fake\n", 0).unwrap();
    assert_eq!(log.progress().unwrap().0, 0);
    producer.write_all(b"real\n").unwrap();
    log.release_writer().unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while log.progress().unwrap().0 != 5 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let observed = log.progress().unwrap();
    outside.write_all_at(b"fake\n", 0).unwrap();
    assert_eq!(log.progress().unwrap(), observed);
    let expected: String = Sha256::digest(b"real\n")
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(observed.1, expected);
    close(outside).unwrap();
    close(producer).unwrap();
    let completed = log.finish().await.unwrap();
    assert!(completed.complete);
    assert_eq!(completed.sha256, observed.1);
    // File readback must be matched to the observed pipe digest by the owner;
    // producer progress never adopts an outside file edit as received data.
    assert_eq!(fixture.bytes(), b"fake\n");
}

#[tokio::test]
async fn releasing_the_parent_writer_makes_producer_eof_observable_before_finish() {
    let fixture = Fixture::new();
    let mut log = Log::start(fixture.file(), 64).unwrap();
    let mut producer = log.writer().unwrap();
    log.release_writer().unwrap();
    assert!(log.writer().is_err());
    producer.write_all(b"ready\n").unwrap();
    tokio::task::yield_now().await;
    assert!(!log.drainer_finished());
    close(producer).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !log.drainer_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let completed = log.finish().await.unwrap();
    assert!(completed.complete);
    assert_eq!(completed.bytes, 6);
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

#[tokio::test(flavor = "current_thread")]
async fn small_unboxed_log_future_keeps_pending_pipe_bytes_through_later_polls() {
    use tokio::io::AsyncWriteExt;

    let fixture = Fixture::new();
    let payload = vec![b'L'; 2 * 65536 + 37];
    let (sender, reader) = tokio::net::unix::pipe::pipe().unwrap();
    let (stop, stopped) = oneshot::channel();
    let progress = Arc::new(Mutex::new(Progress {
        bytes: 0,
        digest: Sha256::new(),
    }));
    let reading = read_log(
        reader,
        fixture.file(),
        payload.len() as u64,
        stopped,
        progress.clone(),
    );
    let compact = std::mem::size_of_val(&reading) < 32 * 1024;
    tokio::pin!(reading);
    // Lose only a temporary poll branch. The same owning operation remains
    // pinned and retains its original receiver/output and nonempty read buffer.
    let premature = tokio::select! {
        biased;
        result = &mut reading => Some(result),
        _ = tokio::task::yield_now() => None,
    };
    let stayed_pending = premature.is_none();
    let initial_bytes = progress.lock().unwrap().bytes;
    let mut sender = Some(sender);
    let attempted = if let Some(done) = premature {
        let closed = sender
            .take()
            .unwrap()
            .into_nonblocking_fd()
            .and_then(|fd| nix::unistd::close(fd).map_err(std::io::Error::from));
        Ok((done, closed))
    } else {
        let producing = async {
            tokio::task::yield_now().await;
            let written = sender.as_mut().unwrap().write_all(&payload).await;
            let closed = sender
                .take()
                .unwrap()
                .into_nonblocking_fd()
                .and_then(|fd| nix::unistd::close(fd).map_err(std::io::Error::from));
            written.and(closed)
        };
        tokio::time::timeout(Duration::from_secs(4), async {
            tokio::join!(&mut reading, producing)
        })
        .await
    };
    // A timeout cancels only the borrowing producer; explicitly close its
    // original sender and wake/join the retained reader before any assertion.
    let sender_closed = sender
        .take()
        .map(|sender| {
            sender
                .into_nonblocking_fd()
                .and_then(|fd| nix::unistd::close(fd).map_err(std::io::Error::from))
        })
        .transpose();
    drop(stop);
    let completed_in_time = attempted.is_ok();
    let (done, written) = match attempted {
        Ok(done) => done,
        Err(_) => (reading.await, Ok(())),
    };
    sender_closed.unwrap();
    written.unwrap();
    let done = done.unwrap();
    assert!(compact && completed_in_time);
    assert!(stayed_pending);
    assert_eq!(initial_bytes, 0);
    assert!(done.complete);
    assert_eq!(done.bytes, payload.len() as u64);
    assert_eq!(fixture.bytes(), payload);
    let expected: String = Sha256::digest(&payload)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(done.sha256, expected);
    assert_eq!(progress.lock().unwrap().bytes, done.bytes);
}
