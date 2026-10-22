//! Safe ownership of the parent endpoint, separate from the reserved child group.

use super::Control;
use anyhow::{Result, ensure};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use tokio::io::unix::AsyncFd;

pub(super) struct NativeControl(AsyncFd<UnixStream>);
impl NativeControl {
    pub(super) fn new(stream: UnixStream) -> Result<Self> {
        if let Err(error) = stream.set_nonblocking(true) {
            let _ = nix::unistd::close(stream);
            return Err(error.into());
        }
        match AsyncFd::try_new(stream) {
            Ok(stream) => Ok(Self(stream)),
            Err(error) => {
                let (stream, error) = error.into_parts();
                let _ = nix::unistd::close(stream);
                Err(error.into())
            }
        }
    }
}

#[async_trait::async_trait]
impl Control for NativeControl {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize> {
        loop {
            let mut ready = self.0.readable_mut().await?;
            match ready.try_io(|stream| stream.get_mut().read(buffer)) {
                Ok(result) => return Ok(result?),
                Err(_) => continue,
            }
        }
    }
    async fn write(&mut self, mut bytes: &[u8]) -> Result<()> {
        while !bytes.is_empty() {
            let mut ready = self.0.writable_mut().await?;
            match ready.try_io(|stream| stream.get_mut().write(bytes)) {
                Ok(result) => {
                    let count = result?;
                    ensure!(count != 0, "Control peer closed");
                    bytes = &bytes[count..];
                }
                Err(_) => continue,
            }
        }
        Ok(())
    }
    fn finish(self: Box<Self>) -> Result<()> {
        nix::unistd::close(self.0.into_inner())?;
        Ok(())
    }
}
