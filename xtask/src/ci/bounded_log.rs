//! An admitted private file receives one bounded combined output pipe.
//!
//! Callers retain path/namespace authority. The core owns only the supplied file,
//! pipe and asynchronous reader. Finish after child-group retirement, then gate
//! publication on complete EOF and successful explicit descriptor closes.

use crate::tooling::{Failure, Result};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata},
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{io::unix::AsyncFd, sync::oneshot, task::JoinHandle};

pub(super) struct Finished {
    pub sha256: String,
    pub bytes: u64,
    pub complete: bool,
    pub metadata: Metadata,
}
pub(super) struct Log {
    #[cfg(test)]
    original_pipe_fds: [i32; 2],
    writer: Option<File>,
    task: Option<JoinHandle<Result<Finished>>>,
    stop: Option<oneshot::Sender<()>>,
    failed: Arc<AtomicBool>,
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
        let reader = match AsyncFd::try_new(read) {
            Ok(reader) => reader,
            Err(error) => {
                let (read, _) = error.into_parts();
                let _ = close(read);
                let _ = close(write);
                let _ = close(output);
                return Err(failure());
            }
        };
        let (stop, stopped) = oneshot::channel();
        let failed = Arc::new(AtomicBool::new(false));
        let flag = failed.clone();
        let task = tokio::spawn(async move {
            let result = read_log(reader, output, maximum, stopped).await;
            if !result.as_ref().is_ok_and(|value| value.complete) {
                flag.store(true, Ordering::SeqCst);
            }
            result
        });
        Ok(Self {
            #[cfg(test)]
            original_pipe_fds,
            writer: Some(write),
            task: Some(task),
            stop: Some(stop),
            failed,
        })
    }
    pub fn writer(&self) -> Result<File> {
        self.writer
            .as_ref()
            .ok_or_else(failure)?
            .try_clone()
            .map_err(|_| failure())
    }
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::SeqCst)
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
    reader: AsyncFd<File>,
    mut output: File,
    maximum: u64,
    mut stop: oneshot::Receiver<()>,
) -> Result<Finished> {
    let mut digest = Sha256::new();
    let mut bytes = 0_u64;
    let mut complete = false;
    let result: Result<()> = async {
        let mut buffer = [0; 65536];
        loop {
            let mut ready = tokio::select! {
                biased;
                _ = &mut stop => return Ok(()),
                ready = reader.readable() => ready.map_err(|_| failure())?,
            };
            let size = match ready.try_io(|fd| {
                let mut file = fd.get_ref();
                file.read(&mut buffer)
            }) {
                Ok(size) => size.map_err(|_| failure())?,
                Err(_) => continue,
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
    let pipe_closed = close(reader.into_inner());
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
