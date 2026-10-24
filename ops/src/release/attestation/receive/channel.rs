//! Bounded borrowed standard descriptors; no blocking Tokio stdin worker survives cancellation.
use super::super::control::{self, Cause, Owner, Result, Window};
use std::{io, os::fd::AsFd, time::Duration};

pub(super) trait Channel {
    async fn line(&mut self, owner: &mut Owner) -> Result<Vec<u8>>;
    async fn write(&mut self, bytes: &[u8], final_output: bool, owner: &mut Owner) -> Result<()>;
    fn finish(&mut self, owner: &mut Owner);
}
pub(super) struct Standard {
    input: io::Stdin,
    output: io::Stdout,
    flags: Option<(rustix::fs::OFlags, rustix::fs::OFlags)>,
}
impl Standard {
    pub fn new(owner: &mut Owner) -> Result<Self> {
        use io::Write;
        owner.check(Window::Body)?;
        let input = io::stdin();
        let output = io::stdout();
        control::io(owner, output.lock().flush())?;
        let input_flags = rustix::fs::fcntl_getfl(input.as_fd()).map_err(|_| Cause::Io)?;
        let output_flags = rustix::fs::fcntl_getfl(output.as_fd()).map_err(|_| Cause::Io)?;
        rustix::fs::fcntl_setfl(input.as_fd(), input_flags | rustix::fs::OFlags::NONBLOCK)
            .map_err(|_| Cause::Io)?;
        if rustix::fs::fcntl_setfl(output.as_fd(), output_flags | rustix::fs::OFlags::NONBLOCK)
            .is_err()
        {
            owner.record(Cause::Io);
            let input_restored = rustix::fs::fcntl_setfl(input.as_fd(), input_flags);
            let output_restored = rustix::fs::fcntl_setfl(output.as_fd(), output_flags);
            if input_restored.is_err() || output_restored.is_err() {
                owner.record(Cause::Close);
            }
            return Err(Cause::Io);
        }
        Ok(Self {
            input,
            output,
            flags: Some((input_flags, output_flags)),
        })
    }
}
impl Channel for Standard {
    async fn line(&mut self, owner: &mut Owner) -> Result<Vec<u8>> {
        let mut line = Vec::with_capacity(256);
        loop {
            owner.check(Window::Body)?;
            // One byte avoids consuming the next frame or requiring transport EOF.
            let mut byte = [0];
            match rustix::io::read(self.input.as_fd(), &mut byte) {
                Ok(0) => return control::failed(owner, Cause::Input),
                Ok(_) => {
                    owner.check(Window::Body)?;
                    line.push(byte[0]);
                    if line.len() > 16384 {
                        return control::failed(owner, Cause::Bounds);
                    }
                    if byte[0] == b'\n' {
                        return Ok(line);
                    }
                }
                Err(rustix::io::Errno::INTR) => {}
                Err(rustix::io::Errno::AGAIN) => tokio::time::sleep(Duration::from_millis(5)).await,
                Err(_) => return control::failed(owner, Cause::Io),
            }
        }
    }
    async fn write(&mut self, bytes: &[u8], final_output: bool, owner: &mut Owner) -> Result<()> {
        let mut offset = 0;
        while offset < bytes.len() {
            if final_output {
                owner.final_work()?;
            } else {
                owner.check(Window::Body)?;
            }
            match rustix::io::write(self.output.as_fd(), &bytes[offset..]) {
                Ok(0) => return control::failed(owner, Cause::Io),
                Ok(count) => offset += count,
                Err(rustix::io::Errno::INTR) => {}
                Err(rustix::io::Errno::AGAIN) => tokio::time::sleep(Duration::from_millis(5)).await,
                Err(_) => return control::failed(owner, Cause::Io),
            }
        }
        if final_output {
            owner.final_work()
        } else {
            owner.check(Window::Body)
        }
    }
    fn finish(&mut self, owner: &mut Owner) {
        if let Some((input, output)) = self.flags.take() {
            let first = rustix::fs::fcntl_setfl(self.input.as_fd(), input);
            let second = rustix::fs::fcntl_setfl(self.output.as_fd(), output);
            if first.is_err() || second.is_err() {
                owner.record(Cause::Close);
            }
        }
    }
}
