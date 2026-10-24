//! An admitted private file receives one bounded combined output pipe.
//!
//! Callers retain path/namespace authority. The core owns only the supplied file,
//! pipe and asynchronous reader. Finish after child-group retirement, then gate
//! publication on complete EOF and successful explicit descriptor closes.

use crate::tooling::{Failure, Result};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata},
    io::Write,
    os::unix::fs::MetadataExt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{net::unix::pipe::Receiver, sync::oneshot, task::JoinHandle};

pub(super) struct Finished {
    pub sha256: String,
    pub bytes: u64,
    pub complete: bool,
    pub metadata: Metadata,
}
pub(super) struct Log {
    maximum: u64,
    #[cfg(test)]
    original_pipe_fds: [i32; 2],
    writer: Option<File>,
    task: Option<JoinHandle<Result<Finished>>>,
    stop: Option<oneshot::Sender<()>>,
    failed: Arc<AtomicBool>,
    progress: Arc<Mutex<Progress>>,
}
struct Progress {
    bytes: u64,
    digest: Sha256,
}
impl std::fmt::Debug for Log {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("BoundedCombinedLog")
    }
}
fn failure() -> Failure {
    Failure::new(1, "Bounded combined log operation failed.")
}
fn close(file: File) -> Result<()> {
    nix::unistd::close(file).map_err(|_| failure())
}
impl Log {
    /// Take an already-admitted empty, private, regular output descriptor.
    /// This does not authenticate its pathname, source or namespace.
    pub fn start(output: File, maximum: u64) -> Result<Self> {
        let admission = (|| {
            let metadata = output.metadata().map_err(|_| failure())?;
            if !metadata.is_file()
                || metadata.len() != 0
                || metadata.mode() & 0o777 != 0o600
                || metadata.uid() != rustix::process::getuid().as_raw()
            {
                return Err(failure());
            }
            Ok(())
        })();
        if let Err(error) = admission {
            close(output)?;
            return Err(error);
        }
        let (read, write) = match nix::unistd::pipe() {
            Ok(value) => value,
            Err(_) => {
                close(output)?;
                return Err(failure());
            }
        };
        let read = File::from(read);
        let write = File::from(write);
        #[cfg(test)]
        let original_pipe_fds = {
            use std::os::fd::AsRawFd;
            [read.as_raw_fd(), write.as_raw_fd()]
        };
        let admission: Result<()> = (|| {
            for file in [&read, &write] {
                rustix::io::fcntl_setfd(file, rustix::io::FdFlags::CLOEXEC)
                    .map_err(|_| failure())?;
            }
            let flags = rustix::fs::fcntl_getfl(&read).map_err(|_| failure())?;
            rustix::fs::fcntl_setfl(&read, flags | rustix::fs::OFlags::NONBLOCK)
                .map_err(|_| failure())
        })();
        if let Err(error) = admission {
            let _ = close(read);
            let _ = close(write);
            let _ = close(output);
            return Err(error);
        }
        // This safe pipe conversion retains the same owned descriptor. On a
        // conversion error Tokio drops it; that is not a checked-close proof.
        let reader = match Receiver::from_file(read) {
            Ok(reader) => reader,
            Err(_) => {
                let _ = close(write);
                let _ = close(output);
                return Err(failure());
            }
        };
        let (stop, stopped) = oneshot::channel();
        let failed = Arc::new(AtomicBool::new(false));
        let flag = failed.clone();
        let progress = Arc::new(Mutex::new(Progress {
            bytes: 0,
            digest: Sha256::new(),
        }));
        let observed = progress.clone();
        let task = tokio::spawn(async move {
            let result = read_log(reader, output, maximum, stopped, observed).await;
            if !result.as_ref().is_ok_and(|value| value.complete) {
                flag.store(true, Ordering::SeqCst);
            }
            result
        });
        Ok(Self {
            maximum,
            #[cfg(test)]
            original_pipe_fds,
            writer: Some(write),
            task: Some(task),
            stop: Some(stop),
            failed,
            progress,
        })
    }
    pub fn writer(&self) -> Result<File> {
        self.writer
            .as_ref()
            .ok_or_else(failure)?
            .try_clone()
            .map_err(|_| failure())
    }
    pub fn maximum(&self) -> u64 {
        self.maximum
    }
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::SeqCst)
    }
    /// After the owned child has inherited its writer, close this parent's
    /// original writer so a later child EOF can be observed during supervision.
    /// Failure remains negative; finish still settles the independent reader.
    pub fn release_writer(&mut self) -> Result<()> {
        self.writer.take().map(close).transpose().map(|_| ())
    }
    /// Observe reader-task termination only to refuse a lost interactive lease.
    /// This is not proof of successful EOF, checked closes or completed output;
    /// the caller must still consume finish after owned group retirement.
    pub fn drainer_finished(&self) -> bool {
        self.task.as_ref().is_some_and(JoinHandle::is_finished)
    }
    /// Snapshot the exact prefix accepted from this original pipe after its
    /// file writes. This can bind a live lease response to observed producer
    /// bytes; it is not durable-output, EOF or final-close evidence.
    pub fn progress(&self) -> Result<(u64, String)> {
        if self.failed() {
            return Err(failure());
        }
        let progress = self.progress.lock().map_err(|_| failure())?;
        let sha256 = progress
            .digest
            .clone()
            .finalize()
            .iter()
            .map(|value| format!("{value:02x}"))
            .collect();
        Ok((progress.bytes, sha256))
    }
    #[cfg(test)]
    pub fn pipe_fds(&self) -> [i32; 2] {
        self.original_pipe_fds
    }
    /// Call only after all owned CLI groups are retired/reaped. A separated
    /// root/session descendant can hold a pipe: never let its EOF block us.
    pub async fn finish(mut self) -> Result<Finished> {
        let closed = self.writer.take().map(close).transpose();
        let mut task = self.task.take().unwrap();
        let result = match tokio::time::timeout(Duration::from_secs(2), &mut task).await {
            Ok(result) => result,
            Err(_) => {
                if let Some(stop) = self.stop.take() {
                    let _ = stop.send(());
                }
                match tokio::time::timeout(Duration::from_secs(1), &mut task).await {
                    Ok(result) => result,
                    Err(_) => {
                        task.abort();
                        return Err(failure());
                    }
                }
            }
        };
        closed?;
        result.map_err(|_| failure())?
    }
}
impl Drop for Log {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
async fn read_log(
    reader: Receiver,
    mut output: File,
    maximum: u64,
    mut stop: oneshot::Receiver<()>,
    progress: Arc<Mutex<Progress>>,
) -> Result<Finished> {
    let mut digest = Sha256::new();
    let mut bytes = 0_u64;
    let mut complete = false;
    let result: Result<()> = async {
        let mut buffer = [0; 65536];
        loop {
            tokio::select! {
                biased;
                _ = &mut stop => return Ok(()),
                ready = reader.readable() => ready.map_err(|_| failure())?,
            };
            let size = match reader.try_read(&mut buffer) {
                Ok(size) => size,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(_) => return Err(failure()),
            };
            if size == 0 {
                complete = true;
                return Ok(());
            }
            let allowed = maximum.saturating_sub(bytes).min(size as u64) as usize;
            output
                .write_all(&buffer[..allowed])
                .map_err(|_| failure())?;
            digest.update(&buffer[..allowed]);
            bytes += allowed as u64;
            {
                let mut observed = progress.lock().map_err(|_| failure())?;
                observed.bytes = bytes;
                observed.digest = digest.clone();
            }
            if allowed != size {
                return Ok(());
            }
        }
    }
    .await;
    let flushed = output
        .flush()
        .and_then(|()| output.sync_all())
        .map_err(|_| failure());
    let metadata = output.metadata().map_err(|_| failure());
    // A deregistration failure consumes the receiver and uses Tokio's Drop
    // path. Keep that result negative while still closing the output peer.
    let pipe_closed = reader
        .into_nonblocking_fd()
        .map_err(|_| failure())
        .and_then(|file| nix::unistd::close(file).map_err(|_| failure()));
    let log_closed = close(output);
    result?;
    flushed?;
    pipe_closed?;
    log_closed?;
    let metadata = metadata?;
    if metadata.len() != bytes {
        return Err(failure());
    }
    Ok(Finished {
        sha256: digest
            .finalize()
            .iter()
            .map(|v| format!("{v:02x}"))
            .collect(),
        bytes,
        complete,
        metadata,
    })
}

#[cfg(test)]
#[path = "bounded_log_tests.rs"]
mod tests;
