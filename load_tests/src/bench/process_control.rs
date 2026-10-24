//! Safe ownership of the parent endpoint, separate from the reserved child group.

use super::{Control, ControlWriteTimedOut};
use anyhow::{Result, ensure};
use std::os::unix::net::UnixStream;
use std::time::Duration;
use tokio::net::UnixStream as TokioUnixStream;
use tokio::time::Instant;

pub(super) struct NativeControl {
    socket: TokioUnixStream,
    write_failed: bool,
}
impl NativeControl {
    pub(super) fn new(stream: UnixStream) -> Result<Self> {
        if let Err(error) = stream.set_nonblocking(true) {
            let _ = nix::unistd::close(stream);
            return Err(error.into());
        }
        // The safe conversion owns the same socket FD. On error Tokio drops
        // that owner; the negative result is not a checked-close proof.
        match TokioUnixStream::from_std(stream) {
            Ok(socket) => Ok(Self {
                socket,
                write_failed: false,
            }),
            Err(error) => Err(error.into()),
        }
    }
    async fn bounded(&mut self, mut bytes: &[u8], limit: Duration) -> Result<()> {
        let deadline = Instant::now()
            .checked_add(limit)
            .ok_or_else(|| anyhow::anyhow!("Invalid owned control write limit"))?;
        while !bytes.is_empty() {
            if Instant::now() >= deadline {
                return Err(ControlWriteTimedOut.into());
            }
            // Only readiness waits are timed out. try_write is a synchronous
            // NONBLOCK syscall, so no write future or worker can be abandoned.
            tokio::time::timeout_at(deadline, self.socket.writable())
                .await
                .map_err(|_| ControlWriteTimedOut)??;
            if Instant::now() >= deadline {
                return Err(ControlWriteTimedOut.into());
            }
            match self.socket.try_write(bytes) {
                Ok(count) => {
                    ensure!(count != 0, "Control peer closed");
                    bytes = &bytes[count..];
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(error) => return Err(error.into()),
            }
        }
        if Instant::now() >= deadline {
            return Err(ControlWriteTimedOut.into());
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Control for NativeControl {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize> {
        loop {
            self.socket.readable().await?;
            match self.socket.try_read(buffer) {
                Ok(count) => return Ok(count),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(error) => return Err(error.into()),
            }
        }
    }
    async fn write(&mut self, mut bytes: &[u8]) -> Result<()> {
        ensure!(!self.write_failed, "Owned control write already failed");
        while !bytes.is_empty() {
            self.socket.writable().await?;
            match self.socket.try_write(bytes) {
                Ok(count) => {
                    ensure!(count != 0, "Control peer closed");
                    bytes = &bytes[count..];
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
    async fn write_bounded(&mut self, bytes: &[u8], limit: Duration) -> Result<()> {
        ensure!(!self.write_failed, "Owned control write already failed");
        let result = self.bounded(bytes, limit).await;
        if result.is_err() {
            self.write_failed = true;
            // Preserve the first failure while attempting the half-close. No
            // operation remains in flight; final descriptor close is separate.
            let _ = rustix::net::shutdown(&self.socket, rustix::net::Shutdown::Write);
        }
        result
    }
    fn finish(self: Box<Self>) -> Result<()> {
        // A failed deregistration consumes/drops Tokio's owner and stays
        // negative. Only a returned std owner reaches the explicit close.
        nix::unistd::close(self.socket.into_std()?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[tokio::test]
    async fn a_full_peer_buffer_times_out_half_closes_and_never_retries_a_failed_frame() {
        let (mut parent, mut peer) = UnixStream::pair().unwrap();
        parent.set_nonblocking(true).unwrap();
        let mut prefixed = 0;
        loop {
            match parent.write(&[b'p'; 8192]) {
                Ok(count) => {
                    assert!(count > 0);
                    prefixed += count;
                    assert!(
                        prefixed < 8 * 1024 * 1024,
                        "Fixture buffer exceeds its bound"
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("Owned socket fixture could not prefill: {error}"),
            }
        }
        assert!(prefixed > 0);
        let mut control = NativeControl::new(parent).unwrap();
        let bytes = vec![b'x'; 8 * 1024 * 1024];
        let error = control
            .write_bounded(&bytes, Duration::from_millis(20))
            .await
            .unwrap_err();
        assert_eq!(super::super::failure_status(&error), Some(124));
        assert!(control.write(b"RETRY").await.is_err());
        assert!(
            control
                .write_bounded(b"RETRY", Duration::from_secs(1))
                .await
                .is_err()
        );
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mut received = Vec::new();
        // EOF proves the write half was closed while the retained parent FD
        // remains alive. Scheduling need not permit any timed-write progress.
        peer.read_to_end(&mut received).unwrap();
        assert!(received.len() >= prefixed && received.len() < prefixed + bytes.len());
        assert!(received[..prefixed].iter().all(|byte| *byte == b'p'));
        assert!(received[prefixed..].iter().all(|byte| *byte == b'x'));
        Box::new(control).finish().unwrap();
        nix::unistd::close(peer).unwrap();
    }

    #[tokio::test]
    async fn a_closed_peer_fails_without_a_retry_or_a_successful_frame() {
        let (parent, peer) = UnixStream::pair().unwrap();
        nix::unistd::close(peer).unwrap();
        let mut control = NativeControl::new(parent).unwrap();
        let error = control
            .write_bounded(b"ack\n", Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(!error.is::<ControlWriteTimedOut>());
        assert!(control.write(b"retry").await.is_err());
        Box::new(control).finish().unwrap();
    }
}
