//! Fixed tool originals, explicit ancillary owners and bounded nonblocking streams.
use super::{Cause, MAX_FRAME, MAX_STREAM, Owner, Result, Window, fail, identity, protocol::Frame};
use rustix::net::{
    self, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, SocketType,
};
use sha2::{Digest, Sha256};
use std::{
    io::{IoSlice, IoSliceMut, SeekFrom},
    mem::MaybeUninit,
    os::fd::{AsRawFd, BorrowedFd, OwnedFd},
    os::unix::fs::MetadataExt,
    path::Path,
    time::{Duration, Instant},
};

pub(super) fn checkpoint(
    owner: &mut Owner,
    event: crate::files::SnapshotEvent,
    observation: std::result::Result<(), crate::Error>,
) -> std::result::Result<(), crate::Error> {
    if let Err(error) = observation {
        owner.record(match (event, error) {
            (_, crate::Error::Changed) => Cause::Changed,
            (crate::files::SnapshotEvent::Close, _) => Cause::Close,
            (crate::files::SnapshotEvent::Check, _) => Cause::Policy,
            _ => Cause::Io,
        });
        return Err(error);
    }
    owner.check(Window::Body).map_err(|_| crate::Error::Input)
}
pub(super) struct Tool {
    original: Option<crate::files::Snapshot>,
    pub hash: String,
    pub node: (u64, u64),
}
impl Tool {
    pub fn open(path: &str, maximum: u64, mode: u32, owner: &mut Owner) -> Result<Self> {
        let original =
            crate::files::Snapshot::open_checked(Path::new(path), maximum, |event, value| {
                checkpoint(owner, event, value)
            })
            .map_err(|_| owner.error().unwrap_or(Cause::Io))?;
        let mut tool = Self {
            original: Some(original),
            hash: String::new(),
            node: (0, 0),
        };
        let result = tool.admit(mode, owner);
        if let Err(first) = result {
            owner.record(first);
            tool.finish(owner);
            return Err(first);
        }
        Ok(tool)
    }
    fn admit(&mut self, mode: u32, owner: &mut Owner) -> Result<()> {
        owner.check(Window::Body)?;
        let file = self.original.as_mut().ok_or(Cause::Input)?.file_mut();
        let metadata = file.metadata().map_err(|_| Cause::Io)?;
        if metadata.uid() != 0
            || metadata.gid() != 0
            || metadata.nlink() != 1
            || metadata.mode() & 0o7777 != mode
        {
            return fail(owner, Cause::Policy);
        }
        self.node = (metadata.dev(), metadata.ino());
        let mut digest = Sha256::new();
        let mut block = [0u8; 65536];
        loop {
            let result = self
                .original
                .as_mut()
                .ok_or(Cause::Input)?
                .read_checked(&mut block, |event, value| checkpoint(owner, event, value));
            match result {
                Ok(0) => break,
                Ok(n) => digest.update(&block[..n]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return fail(owner, owner.error().unwrap_or(Cause::Io)),
            }
        }
        self.hash = hex(&digest.finalize());
        self.original
            .as_mut()
            .ok_or(Cause::Input)?
            .seek_checked(SeekFrom::Start(0), |event, value| {
                checkpoint(owner, event, value)
            })
            .map_err(|_| owner.error().unwrap_or(Cause::Io))?;
        Ok(())
    }
    pub fn outer_alias(&mut self, proc: &identity::Proc, owner: &mut Owner) -> Result<String> {
        if !proc.outer() {
            return fail(owner, Cause::Policy);
        }
        self.check(owner)?;
        Ok(format!(
            "/proc/{}/fd/{}",
            rustix::process::getpid().as_raw_nonzero().get(),
            self.original
                .as_mut()
                .ok_or(Cause::Input)?
                .file_mut()
                .as_raw_fd()
        ))
    }
    pub fn check(&mut self, owner: &mut Owner) -> Result<()> {
        let file = self.original.as_mut().ok_or(Cause::Input)?;
        file.seek_checked(SeekFrom::Start(0), |event, value| {
            checkpoint(owner, event, value)
        })
        .map_err(|_| owner.error().unwrap_or(Cause::Io))?;
        let mut digest = Sha256::new();
        let mut block = [0u8; 65536];
        loop {
            match file.read_checked(&mut block, |event, value| checkpoint(owner, event, value)) {
                Ok(0) => break,
                Ok(n) => digest.update(&block[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return fail(owner, owner.error().unwrap_or(Cause::Io)),
            }
        }
        if hex(&digest.finalize()) != self.hash {
            return fail(owner, Cause::Changed);
        }
        Ok(())
    }
    pub fn check_final(&mut self, owner: &mut Owner) -> Result<()> {
        let file = self.original.as_mut().ok_or(Cause::Input)?;
        let mut scope = |event, value: std::result::Result<(), crate::Error>| {
            if let Err(error) = value {
                owner.record(match (event, error) {
                    (_, crate::Error::Changed) => Cause::Changed,
                    (crate::files::SnapshotEvent::Close, _) => Cause::Close,
                    (crate::files::SnapshotEvent::Check, _) => Cause::Policy,
                    _ => Cause::Io,
                });
                return Err(error);
            }
            owner.final_work().map_err(|_| crate::Error::Input)
        };
        file.seek_checked(SeekFrom::Start(0), &mut scope)
            .map_err(|_| Cause::Changed)?;
        let mut digest = Sha256::new();
        let mut block = [0u8; 65536];
        loop {
            match file.read_checked(&mut block, &mut scope) {
                Ok(0) => break,
                Ok(n) => digest.update(&block[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Err(Cause::Changed),
            }
        }
        if hex(&digest.finalize()) != self.hash {
            return fail(owner, Cause::Changed);
        }
        Ok(())
    }
    pub fn finish(mut self, owner: &mut Owner) -> bool {
        self.original.take().is_none_or(|file| {
            file.finish_checked(|event, value| {
                if let Err(error) = value {
                    owner.record(match (event, error) {
                        (_, crate::Error::Changed) => Cause::Changed,
                        (crate::files::SnapshotEvent::Close, _) => Cause::Close,
                        (crate::files::SnapshotEvent::Check, _) => Cause::Policy,
                        _ => Cause::Io,
                    });
                    return Err(error);
                }
                owner.final_work().map_err(|_| crate::Error::Input)
            })
            .is_ok()
        })
    }
}
pub(super) fn self_image(tool: &Tool, owner: &mut Owner) -> Result<()> {
    owner.check(Window::Body)?;
    let fd = rustix::fs::open(
        "/proc/self/exe",
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|_| Cause::Io)?;
    image_identity(fd, tool.node, owner)
}
pub(super) fn image_identity(fd: OwnedFd, expected: (u64, u64), owner: &mut Owner) -> Result<()> {
    let body = rustix::fs::fstat(&fd)
        .map_err(|_| Cause::Io)
        .and_then(|value| {
            if (value.st_dev, value.st_ino) != expected {
                Err(Cause::Changed)
            } else {
                Ok(())
            }
        });
    if let Err(first) = body {
        owner.record(first);
    }
    let closed = identity::close(owner, fd);
    body?;
    if !closed {
        return Err(Cause::Close);
    }
    owner.check(Window::Body)
}

pub(super) struct Packet {
    pub bytes: Vec<u8>,
    pub rights: Vec<OwnedFd>,
    pub credential: Option<net::UCred>,
}
impl Packet {
    pub fn finish(self, owner: &mut Owner) -> bool {
        identity::close_all(owner, self.rights)
    }
}
pub(super) struct Channel<'fd> {
    fd: BorrowedFd<'fd>,
    flags: rustix::fs::OFlags,
    credentials: bool,
}
impl<'fd> Channel<'fd> {
    pub fn new(fd: BorrowedFd<'fd>, credentials: bool, owner: &mut Owner) -> Result<Self> {
        owner.check(Window::Body)?;
        if net::sockopt::socket_type(fd).map_err(|_| Cause::Io)? != SocketType::SEQPACKET {
            return fail(owner, Cause::Policy);
        }
        if credentials {
            net::sockopt::set_socket_passcred(fd, true).map_err(|_| Cause::Io)?;
        }
        // In the inner PID namespace, ancestor credentials can encode PID0.
        // Never ask locked UCred<NonZeroPid> to decode that direction.
        if net::sockopt::socket_passcred(fd).map_err(|_| Cause::Io)? != credentials {
            return fail(owner, Cause::Policy);
        }
        let flags = rustix::fs::fcntl_getfl(fd).map_err(|_| Cause::Io)?;
        rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK).map_err(|_| Cause::Io)?;
        Ok(Self {
            fd,
            flags,
            credentials,
        })
    }
    fn ready(owner: &mut Owner, end: Instant, cleanup: bool) -> Result<()> {
        if cleanup {
            owner.cleanup_before(end)
        } else {
            owner.check(Window::Body)?;
            owner.cleanup_before(end)
        }
    }
    pub fn attempt(
        &self,
        owner: &mut Owner,
        end: Instant,
        cleanup: bool,
    ) -> Result<Option<Packet>> {
        let original_end = end.min(owner.end());
        Self::ready(owner, original_end, cleanup)?;
        if net::sockopt::socket_passcred(self.fd).map_err(|_| Cause::Io)? != self.credentials {
            return fail(owner, Cause::Changed);
        }
        let mut data = [0u8; MAX_FRAME + 1];
        let mut space =
            [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(4), ScmCredentials(1))];
        let mut ancillary = RecvAncillaryBuffer::new(&mut space);
        let received = loop {
            Self::ready(owner, original_end, cleanup)?;
            match net::recvmsg(
                self.fd,
                &mut [IoSliceMut::new(&mut data)],
                &mut ancillary,
                RecvFlags::DONTWAIT | RecvFlags::CMSG_CLOEXEC,
            ) {
                Ok(value) => break value,
                Err(rustix::io::Errno::INTR) => continue,
                Err(rustix::io::Errno::AGAIN) => return Ok(None),
                Err(_) => return fail(owner, Cause::Io),
            }
        };
        let mut packet = Packet {
            bytes: Vec::new(),
            rights: Vec::new(),
            credential: None,
        };
        let mut valid = true;
        for message in ancillary.drain() {
            match message {
                RecvAncillaryMessage::ScmRights(fds) => packet.rights.extend(fds),
                RecvAncillaryMessage::ScmCredentials(value)
                    if self.credentials && packet.credential.is_none() =>
                {
                    packet.credential = Some(value)
                }
                _ => valid = false,
            }
        }
        valid &= received.bytes > 0
            && received.bytes <= MAX_FRAME
            && !received
                .flags
                .intersects(net::ReturnFlags::TRUNC | net::ReturnFlags::CTRUNC);
        valid &= self.credentials == packet.credential.is_some() && packet.rights.len() <= 2;
        for right in &packet.rights {
            match rustix::io::fcntl_getfd(right) {
                Ok(flags) if flags.contains(rustix::io::FdFlags::CLOEXEC) => {}
                Ok(_) => {
                    owner.record(Cause::Policy);
                    valid = false;
                }
                Err(_) => {
                    owner.record(Cause::Io);
                    valid = false;
                }
            }
        }
        if !valid {
            owner.record(Cause::Policy);
            let _ = packet.finish(owner);
            return Err(Cause::Policy);
        }
        packet.bytes.extend_from_slice(&data[..received.bytes]);
        #[cfg(test)]
        if let Some(elapsed) = RECV_ELAPSED.with(std::cell::Cell::take) {
            owner.elapsed = Some(elapsed);
        }
        if let Err(first) = Self::ready(owner, original_end, cleanup) {
            owner.record(first);
            let _ = packet.finish(owner);
            return Err(first);
        }
        Ok(Some(packet))
    }
    pub fn receive(&self, owner: &mut Owner, end: Instant, cleanup: bool) -> Result<Packet> {
        loop {
            Self::ready(owner, end, cleanup)?;
            if let Some(packet) = self.attempt(owner, end, cleanup)? {
                if let Err(first) = Self::ready(owner, end, cleanup) {
                    owner.record(first);
                    let _ = packet.finish(owner);
                    return Err(first);
                }
                return Ok(packet);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    pub fn send(
        &self,
        frame: &Frame,
        rights: &[BorrowedFd<'_>],
        owner: &mut Owner,
        end: Instant,
        cleanup: bool,
    ) -> Result<()> {
        let bytes = frame.bytes()?;
        if rights.len() > 2 {
            return fail(owner, Cause::Policy);
        }
        loop {
            Self::ready(owner, end, cleanup)?;
            let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(2))];
            let mut ancillary = SendAncillaryBuffer::new(&mut space);
            if !rights.is_empty() && !ancillary.push(SendAncillaryMessage::ScmRights(rights)) {
                return fail(owner, Cause::Bounds);
            }
            match net::sendmsg(
                self.fd,
                &[IoSlice::new(&bytes)],
                &mut ancillary,
                SendFlags::DONTWAIT | SendFlags::NOSIGNAL,
            ) {
                Ok(n) if n == bytes.len() => return Self::ready(owner, end, cleanup),
                Err(rustix::io::Errno::AGAIN) => std::thread::sleep(Duration::from_millis(5)),
                Err(rustix::io::Errno::INTR) => {}
                _ => return fail(owner, Cause::Io),
            }
        }
    }
    pub fn restore(&self, owner: &mut Owner) -> bool {
        let closed = rustix::fs::fcntl_setfl(self.fd, self.flags).is_ok();
        if !closed {
            owner.record(Cause::Close);
        }
        closed
    }
    pub fn finish(self, owner: &mut Owner) -> bool {
        self.restore(owner)
    }
}

pub(super) struct Capture {
    fd: Option<OwnedFd>,
    hash: Sha256,
    pub bytes: u64,
    pub eof: bool,
    pub closed: bool,
    complete: bool,
}
impl Capture {
    pub fn new(fd: OwnedFd, owner: &mut Owner) -> Result<Self> {
        let mut this = Self {
            fd: Some(fd),
            hash: Sha256::new(),
            bytes: 0,
            eof: false,
            closed: false,
            complete: true,
        };
        let result = (|| {
            owner.check(Window::Body)?;
            let fd = this.fd.as_ref().ok_or(Cause::Input)?;
            let flags = rustix::fs::fcntl_getfl(fd).map_err(|_| Cause::Io)?;
            rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK)
                .map_err(|_| Cause::Io)?;
            owner.check(Window::Body)
        })();
        if let Err(first) = result {
            owner.record(first);
            this.finish(owner);
            return Err(first);
        }
        Ok(this)
    }
    pub fn pump(&mut self, owner: &mut Owner, window: Window) -> Result<()> {
        if self.eof {
            return Ok(());
        }
        let mut bytes = [0u8; 65536];
        loop {
            capture_fence(owner, window)?;
            let result = rustix::io::read(self.fd.as_ref().ok_or(Cause::Input)?, &mut bytes);
            #[cfg(test)]
            if let Some(elapsed) = CAPTURE_ELAPSED.with(std::cell::Cell::take) {
                owner.elapsed = Some(elapsed);
            }
            match result {
                Ok(0) => {
                    self.eof = true;
                    return capture_fence(owner, window);
                }
                Ok(n) => {
                    self.bytes = self
                        .bytes
                        .checked_add(u64::try_from(n).map_err(|_| Cause::Bounds)?)
                        .ok_or(Cause::Bounds)?;
                    if let Err(first) = capture_fence(owner, window) {
                        self.complete = false;
                        return Err(first);
                    }
                    if self.bytes > MAX_STREAM {
                        self.complete = false;
                        return fail(owner, Cause::Bounds);
                    }
                    self.hash.update(&bytes[..n]);
                }
                Err(rustix::io::Errno::AGAIN) => return Ok(()),
                Err(rustix::io::Errno::INTR) => {}
                Err(_) => return fail(owner, Cause::Io),
            }
        }
    }
    pub fn digest(&self) -> Option<String> {
        (self.eof && self.complete && self.bytes <= MAX_STREAM)
            .then(|| hex(&self.hash.clone().finalize()))
    }
    pub fn finish(&mut self, owner: &mut Owner) {
        if let Some(fd) = self.fd.take() {
            self.closed = identity::close(owner, fd);
        }
    }
}
pub(super) fn hex(bytes: &[u8]) -> String {
    super::super::files::hex(bytes)
}

#[cfg(test)]
thread_local! {
    pub(super) static CAPTURE_ELAPSED: std::cell::Cell<Option<Duration>> = const {
        std::cell::Cell::new(None)
    };
    pub(super) static RECV_ELAPSED: std::cell::Cell<Option<Duration>> = const {
        std::cell::Cell::new(None)
    };
}

fn capture_fence(owner: &mut Owner, window: Window) -> Result<()> {
    match window {
        Window::Body => owner.check(Window::Body),
        Window::Final => owner.final_work(),
    }
}
