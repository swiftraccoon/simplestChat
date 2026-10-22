//! One bounded pipe combines both streams; no inherited pipe blocks a thread.

use super::{
    files::{self, Directory},
    plan,
    protocol::Intent,
};
use crate::tooling::Result;
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
impl Log {
    pub fn start(parent: &Directory, intent: &Intent, maximum: u64) -> Result<Self> {
        let output = rustix::fs::openat(
            parent.file(),
            plan::LOG,
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| plan::failure())?;
        let admission = (|| {
            let metadata = output.metadata().map_err(|_| plan::failure())?;
            if !metadata.is_file()
                || metadata.len() != 0
                || files::identity(&output)? != intent.log
                || metadata.mode() & 0o777 != 0o600
                || metadata.uid() != rustix::process::getuid().as_raw()
            {
                return Err(plan::failure());
            }
            Ok(())
        })();
        if let Err(error) = admission {
            files::close(output)?;
            return Err(error);
        }
        let (read, write) = match nix::unistd::pipe() {
            Ok(value) => value,
            Err(_) => {
                files::close(output)?;
                return Err(plan::failure());
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
                    .map_err(|_| plan::failure())?;
            }
            let flags = rustix::fs::fcntl_getfl(&read).map_err(|_| plan::failure())?;
            rustix::fs::fcntl_setfl(&read, flags | rustix::fs::OFlags::NONBLOCK)
                .map_err(|_| plan::failure())
        })();
        if let Err(error) = admission {
            let _ = files::close(read);
            let _ = files::close(write);
            let _ = files::close(output);
            return Err(error);
        }
        let reader = match AsyncFd::try_new(read) {
            Ok(reader) => reader,
            Err(error) => {
                let (read, _) = error.into_parts();
                let _ = files::close(read);
                let _ = files::close(write);
                let _ = files::close(output);
                return Err(plan::failure());
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
            .ok_or_else(plan::failure)?
            .try_clone()
            .map_err(|_| plan::failure())
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
        let closed = self.writer.take().map(files::close).transpose();
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
                        return Err(plan::failure());
                    }
                }
            }
        };
        closed?;
        result.map_err(|_| plan::failure())?
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
                ready = reader.readable() => ready.map_err(|_| plan::failure())?,
            };
            let size = match ready.try_io(|fd| {
                let mut file = fd.get_ref();
                file.read(&mut buffer)
            }) {
                Ok(size) => size.map_err(|_| plan::failure())?,
                Err(_) => continue,
            };
            if size == 0 {
                complete = true;
                return Ok(());
            }
            let allowed = maximum.saturating_sub(bytes).min(size as u64) as usize;
            output
                .write_all(&buffer[..allowed])
                .map_err(|_| plan::failure())?;
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
        .map_err(|_| plan::failure());
    let metadata = output.metadata().map_err(|_| plan::failure());
    let pipe_closed = files::close(reader.into_inner());
    let log_closed = files::close(output);
    result?;
    flushed?;
    pipe_closed?;
    log_closed?;
    let metadata = metadata?;
    if metadata.len() != bytes {
        return Err(plan::failure());
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
