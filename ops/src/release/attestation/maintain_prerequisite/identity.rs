//! Kernel-owned namespace/pidfd identity. Frame PIDs never authorize a lookup.
use super::{Cause, Owner, Result, Window, fail};
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    fs::{Mode, OFlags},
};
use std::{
    collections::BTreeMap,
    os::fd::{AsFd, AsRawFd, OwnedFd},
};

const READ_MAX: usize = 65536;
const NSFS_MAGIC: u64 = 0x6e73_6673;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Node {
    dev: u64,
    ino: u64,
}
fn node(fd: impl AsFd) -> Result<Node> {
    let value = rustix::fs::fstat(fd).map_err(|_| Cause::Io)?;
    Ok(Node {
        dev: value.st_dev,
        ino: value.st_ino,
    })
}
pub(super) fn close(owner: &mut Owner, fd: OwnedFd) -> bool {
    let physical = nix::unistd::close(fd).is_ok();
    #[cfg(test)]
    CLOSED.with(|events| events.borrow_mut().push(physical));
    #[cfg(test)]
    let injected = CLOSE_FAULT.with(|fault| fault.replace(false));
    #[cfg(test)]
    let result = physical && !injected;
    #[cfg(not(test))]
    let result = physical;
    if !result {
        owner.record(Cause::Close);
    }
    result
}
pub(super) fn close_all(owner: &mut Owner, fds: Vec<OwnedFd>) -> bool {
    let mut complete = true;
    for fd in fds {
        complete &= close(owner, fd);
    }
    complete
}
fn observed<T>(owner: &mut Owner, result: Result<T>) -> Result<T> {
    result.inspect_err(|&cause| owner.record(cause))
}

pub(super) struct Proc {
    file: Option<OwnedFd>,
    own: Option<OwnedFd>,
    host: Option<OwnedFd>,
    before: Node,
    own_before: Node,
    host_before: Node,
    depth: usize,
}
impl Proc {
    pub fn open(owner: &mut Owner) -> Result<Self> {
        owner.check(Window::Body)?;
        let file = rustix::fs::open(
            "/proc",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|_| Cause::Io)?;
        let mut held = Self {
            file: Some(file),
            own: None,
            host: None,
            before: Node { dev: 0, ino: 0 },
            own_before: Node { dev: 0, ino: 0 },
            host_before: Node { dev: 0, ino: 0 },
            depth: 0,
        };
        let result = held.admit(owner);
        if let Err(first) = result {
            owner.record(first);
            held.finish(owner);
            return Err(first);
        }
        Ok(held)
    }
    fn admit(&mut self, owner: &mut Owner) -> Result<()> {
        owner.check(Window::Body)?;
        self.before = observed(owner, node(self.fd()))?;
        self.check_proc(owner)?;
        self.own = Some(self.open_role("self/ns/pid", owner)?);
        self.own_before = observed(owner, node(self.own.as_ref().ok_or(Cause::Input)?))?;
        self.host = Some(self.open_role("1/ns/pid", owner)?);
        self.host_before = observed(owner, node(self.host.as_ref().ok_or(Cause::Input)?))?;
        let record = self.read("self/status", owner)?;
        let values = fields(&record)?;
        self.depth = numbers(values.get("NSpid").ok_or(Cause::Policy)?)?
            .len()
            .checked_sub(1)
            .ok_or(Cause::Policy)?;
        owner.check(Window::Body)
    }
    pub fn fd(&self) -> &OwnedFd {
        self.file
            .as_ref()
            .expect("original proc is held until finish")
    }
    pub fn outer(&self) -> bool {
        self.depth == 0 && self.own_before == self.host_before
    }
    pub fn self_namespace(&self) -> &OwnedFd {
        self.own.as_ref().expect("admitted namespace is held")
    }
    pub fn check_proc(&self, owner: &mut Owner) -> Result<()> {
        owner.check(Window::Body)?;
        self.raw_proc().inspect_err(|&cause| owner.record(cause))
    }
    fn raw_proc(&self) -> Result<()> {
        let fs = rustix::fs::fstatfs(self.fd()).map_err(|_| Cause::Io)?;
        if fs.f_type != rustix::fs::PROC_SUPER_MAGIC || node(self.fd())? != self.before {
            return Err(Cause::Changed);
        }
        Ok(())
    }
    pub fn guard_realm(&self) -> Result<()> {
        // Cleanup may be late, but never acquires a new capability or trusts a
        // reported PID. These originals establish this actual PID1's own realm.
        if rustix::process::getpid() != rustix::process::Pid::INIT
            || self.own_before == self.host_before
        {
            return Err(Cause::Policy);
        }
        self.raw_proc()?;
        if node(self.self_namespace())? != self.own_before
            || node(self.host.as_ref().ok_or(Cause::Input)?)? != self.host_before
        {
            return Err(Cause::Changed);
        }
        Ok(())
    }
    fn open_role(&self, path: &str, owner: &mut Owner) -> Result<OwnedFd> {
        self.check_proc(owner)?;
        let fd = rustix::fs::openat(
            self.fd(),
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|_| {
            owner.record(Cause::Io);
            Cause::Io
        })?;
        if let Err(first) = owner.check(Window::Body) {
            let _ = close(owner, fd);
            return Err(first);
        }
        Ok(fd)
    }
    pub fn read(&self, path: &str, owner: &mut Owner) -> Result<Vec<u8>> {
        let fd = self.open_role(path, owner)?;
        let result = read_fd(&fd, owner);
        let closed = close(owner, fd);
        let bytes = result?;
        if !closed {
            return Err(Cause::Close);
        }
        Ok(bytes)
    }
    fn namespace(&self, fd: &OwnedFd, owner: &mut Owner) -> Result<Node> {
        self.check_proc(owner)?;
        let before = observed(owner, node(fd))?;
        let fs = rustix::fs::fstatfs(fd).map_err(|_| Cause::Io)?;
        if u64::try_from(fs.f_type).ok() != Some(NSFS_MAGIC) {
            return fail(owner, Cause::Policy);
        }
        let path = format!("self/fd/{}", fd.as_raw_fd());
        let mut link = [0u8; 128];
        let count = rustix::fs::readlinkat_raw(self.fd(), path.as_str(), &mut link)
            .map_err(|_| Cause::Io)?;
        if count == link.len() {
            return fail(owner, Cause::Bounds);
        }
        let value = std::str::from_utf8(&link[..count]).map_err(|_| Cause::Policy)?;
        let inode = value
            .strip_prefix("pid:[")
            .and_then(|v| v.strip_suffix(']'))
            .and_then(|v| v.parse::<u64>().ok());
        if inode != Some(before.ino) || observed(owner, node(fd))? != before {
            return fail(owner, Cause::Changed);
        }
        owner.check(Window::Body)?;
        Ok(before)
    }
    fn pidfd_record(&self, fd: &OwnedFd, owner: &mut Owner) -> Result<Vec<u32>> {
        live(fd, owner)?;
        let path = format!("self/fdinfo/{}", fd.as_raw_fd());
        let record = self.read(&path, owner)?;
        let values = fields(&record)?;
        let ids = numbers(values.get("NSpid").ok_or(Cause::Policy)?)?;
        let pid = numbers(values.get("Pid").ok_or(Cause::Policy)?)?;
        if pid.len() != 1 || pid[0] != ids[0] {
            return fail(owner, Cause::Policy);
        }
        Ok(ids)
    }
    fn join(
        &self,
        namespace: &OwnedFd,
        pidfd: &OwnedFd,
        ids: &[u32],
        owner: &mut Owner,
    ) -> Result<()> {
        let expected = self.namespace(namespace, owner)?;
        live(pidfd, owner)?;
        let path = format!("{}/ns/pid", ids[0]);
        let original = self.open_role(&path, owner)?;
        let body = (|| {
            if self.namespace(&original, owner)? != expected {
                return fail(owner, Cause::Changed);
            }
            live(pidfd, owner)
        })();
        if let Err(cause) = body {
            owner.record(cause);
        }
        let closed = close(owner, original);
        body?;
        if !closed {
            return Err(Cause::Close);
        }
        live(pidfd, owner)
    }
    pub fn sender(
        &self,
        rights: &[OwnedFd],
        credential: rustix::net::UCred,
        owner: &mut Owner,
    ) -> Result<()> {
        if rights.len() != 2
            || credential.uid != rustix::process::Uid::ROOT
            || credential.gid != rustix::process::Gid::ROOT
        {
            return fail(owner, Cause::Policy);
        }
        let ids = self.pidfd_record(&rights[1], owner)?;
        if ids.len() != self.depth + 2
            || ids[self.depth] != credential.pid.as_raw_nonzero().get() as u32
            || ids.last() != Some(&1)
        {
            return fail(owner, Cause::Policy);
        }
        self.join(&rights[0], &rights[1], &ids, owner)
    }
    pub fn ancestor(&self, rights: &[OwnedFd], owner: &mut Owner) -> Result<()> {
        if rights.len() != 2 || self.depth != 1 {
            return fail(owner, Cause::Policy);
        }
        let ids = self.pidfd_record(&rights[1], owner)?;
        if ids.len() != 1 {
            return fail(owner, Cause::Policy);
        }
        self.join(&rights[0], &rights[1], &ids, owner)?;
        if node(&rights[0])? != self.host_before {
            return fail(owner, Cause::Changed);
        }
        self.guard_realm()
    }

    pub fn nested_client(
        &self,
        timeout_node: (u64, u64),
        owner: &mut Owner,
    ) -> Result<Option<Client>> {
        self.guard_realm()?;
        self.check_proc(owner)?;
        let scan = rustix::fs::openat(
            self.fd(),
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| Cause::Io)?;
        let result = self.scan_client(&scan, timeout_node, owner);
        if let Err(cause) = result {
            owner.record(cause);
        }
        let closed = close(owner, scan);
        let found = result?;
        if !closed {
            if let Some(client) = found {
                let _ = client.finish(owner);
            }
            return Err(Cause::Close);
        }
        Ok(found)
    }
    fn scan_client(
        &self,
        scan: &OwnedFd,
        timeout_node: (u64, u64),
        owner: &mut Owner,
    ) -> Result<Option<Client>> {
        let mut buffer = [std::mem::MaybeUninit::uninit(); 16384];
        let mut entries = rustix::fs::RawDir::new(scan, &mut buffer);
        let mut count = 0;
        while let Some(entry) = entries.next() {
            owner.check(Window::Body)?;
            let entry = match entry {
                Ok(value) => value,
                Err(rustix::io::Errno::INTR) => continue,
                Err(_) => return fail(owner, Cause::Io),
            };
            let name = entry.file_name().to_bytes();
            if !name.iter().all(u8::is_ascii_digit) || name.is_empty() {
                continue;
            }
            count += 1;
            if count > 4096 {
                return fail(owner, Cause::Bounds);
            }
            let name = std::str::from_utf8(name).map_err(|_| Cause::Policy)?;
            if let Some(client) = self.client_at(name, timeout_node, owner)? {
                return Ok(Some(client));
            }
        }
        Ok(None)
    }
    fn client_at(
        &self,
        name: &str,
        timeout_node: (u64, u64),
        owner: &mut Owner,
    ) -> Result<Option<Client>> {
        owner.check(Window::Body)?;
        let directory = match rustix::fs::openat(
            self.fd(),
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT | rustix::io::Errno::SRCH) => return Ok(None),
            Err(_) => return fail(owner, Cause::Io),
        };
        let mut client = Client {
            directory: Some(directory),
            held: Vec::new(),
            own: self.own_before,
            timeout: timeout_node,
        };
        let result = client.admit(owner);
        if let Err(first) = result {
            owner.record(first);
        }
        match result {
            Ok(true) => Ok(Some(client)),
            Ok(false) => {
                if !client.finish(owner) {
                    return Err(Cause::Close);
                }
                Ok(None)
            }
            Err(first) => {
                let _ = client.finish(owner);
                Err(first)
            }
        }
    }
    pub fn finish(mut self, owner: &mut Owner) -> bool {
        let mut complete = true;
        for slot in [&mut self.host, &mut self.own, &mut self.file] {
            if let Some(fd) = slot.take() {
                complete &= close(owner, fd);
            }
        }
        complete
    }
}
pub(super) fn live(fd: &OwnedFd, owner: &mut Owner) -> Result<()> {
    loop {
        owner.check(Window::Body)?;
        let mut peers = [PollFd::new(fd, PollFlags::IN)];
        match poll(
            &mut peers,
            Some(&Timespec {
                tv_sec: 0,
                tv_nsec: 0,
            }),
        ) {
            Ok(0) if peers[0].revents().is_empty() => return owner.check(Window::Body),
            Err(rustix::io::Errno::INTR) => {}
            _ => return fail(owner, Cause::Changed),
        }
    }
}
pub(super) fn read_fd(fd: impl AsFd, owner: &mut Owner) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut block = [0u8; 4096];
    loop {
        owner.check(Window::Body)?;
        let result = rustix::io::read(fd.as_fd(), &mut block);
        #[cfg(test)]
        if READ_EXPIRE.with(|value| value.replace(false)) {
            owner.expire_body();
        }
        match result {
            Ok(0) => {
                owner.check(Window::Body)?;
                return Ok(bytes);
            }
            Ok(count) => {
                owner.check(Window::Body)?;
                if bytes.len() + count > READ_MAX {
                    return fail(owner, Cause::Bounds);
                }
                bytes.extend_from_slice(&block[..count]);
            }
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return fail(owner, Cause::Io),
        }
    }
}
pub(super) fn fields(bytes: &[u8]) -> Result<BTreeMap<&str, &str>> {
    let text = std::str::from_utf8(bytes).map_err(|_| Cause::Policy)?;
    let mut fields = BTreeMap::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once(':')
            && fields.insert(key, value.trim()).is_some()
        {
            return Err(Cause::Policy);
        }
    }
    Ok(fields)
}
pub(super) fn numbers(value: &str) -> Result<Vec<u32>> {
    let values = value
        .split_ascii_whitespace()
        .map(|part| {
            if part.is_empty()
                || part.len() > 10
                || part.starts_with('0')
                || !part.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(Cause::Policy);
            }
            part.parse::<u32>()
                .ok()
                .filter(|n| *n <= i32::MAX as u32)
                .ok_or(Cause::Policy)
        })
        .collect::<Result<Vec<_>>>()?;
    if values.is_empty() || values.len() > 32 {
        return Err(Cause::Policy);
    }
    Ok(values)
}

#[cfg(test)]
thread_local! {
    pub(super) static READ_EXPIRE: std::cell::Cell<bool> = const {std::cell::Cell::new(false)};
    pub(super) static CLOSE_FAULT: std::cell::Cell<bool> = const {std::cell::Cell::new(false)};
    pub(super) static CLOSED: std::cell::RefCell<Vec<bool>> = const {
        std::cell::RefCell::new(Vec::new())
    };
}

pub(super) struct Client {
    directory: Option<OwnedFd>,
    held: Vec<OwnedFd>,
    own: Node,
    timeout: (u64, u64),
}
impl Client {
    fn directory(&self) -> &OwnedFd {
        self.directory
            .as_ref()
            .expect("task directory remains through close")
    }
    fn open(&self, name: &str, owner: &mut Owner) -> Result<Option<OwnedFd>> {
        owner.check(Window::Body)?;
        match rustix::fs::openat(
            self.directory(),
            name,
            OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => {
                if let Err(first) = owner.check(Window::Body) {
                    let _ = close(owner, fd);
                    return Err(first);
                }
                Ok(Some(fd))
            }
            Err(rustix::io::Errno::NOENT | rustix::io::Errno::SRCH) => Ok(None),
            Err(_) => fail(owner, Cause::Io),
        }
    }
    fn admit(&mut self, owner: &mut Owner) -> Result<bool> {
        let Some(namespace) = self.open("ns/pid", owner)? else {
            return Ok(false);
        };
        self.held.push(namespace);
        if node(&self.held[0])? != self.own {
            return Ok(false);
        }
        let Some(executable) = self.open("exe", owner)? else {
            return Ok(false);
        };
        self.held.push(executable);
        let executable = node(&self.held[1])?;
        if (executable.dev, executable.ino) != self.timeout {
            return Ok(false);
        }
        let Some(command) = self.open("cmdline", owner)? else {
            return Ok(false);
        };
        self.held.push(command);
        let bytes = read_fd(&self.held[2], owner)?;
        owner.check(Window::Body)?;
        Ok(readonly_client(&bytes))
    }
    pub fn stopped(&self, proc: &Proc, owner: &mut Owner) -> Result<()> {
        proc.guard_realm()?;
        owner.check(Window::Body)?;
        let mut fresh = Vec::new();
        let result = (|| {
            for role in ["status", "exe", "cmdline"] {
                fresh.push(self.open(role, owner)?.ok_or(Cause::Policy)?);
            }
            let status = read_fd(&fresh[0], owner)?;
            let values = fields(&status)?;
            if values
                .get("State")
                .and_then(|value| value.split_ascii_whitespace().next())
                != Some("T")
            {
                return fail(owner, Cause::Policy);
            }
            let actual = node(&fresh[1])?;
            if (actual.dev, actual.ino) != self.timeout {
                return fail(owner, Cause::Changed);
            }
            if !readonly_client(&read_fd(&fresh[2], owner)?) || node(&self.held[0])? != self.own {
                return fail(owner, Cause::Changed);
            }
            owner.check(Window::Body)
        })();
        if let Err(first) = result {
            owner.record(first);
        }
        let closed = close_all(owner, fresh);
        result?;
        if !closed {
            return Err(Cause::Close);
        }
        Ok(())
    }
    pub fn finish(mut self, owner: &mut Owner) -> bool {
        let mut closed = close_all(owner, std::mem::take(&mut self.held));
        if let Some(fd) = self.directory.take() {
            closed &= close(owner, fd);
        }
        closed
    }
}
pub(super) fn readonly_client(bytes: &[u8]) -> bool {
    let words: Vec<&[u8]> = bytes.split(|b| *b == 0).collect();
    let expected: [&[u8]; 11] = [
        b"timeout",
        b"--signal=TERM",
        b"--kill-after=5s",
        b"180s",
        b"/usr/local/bin/simplestchat-public",
        b"ps",
        b"--status",
        b"running",
        b"--quiet",
        b"simplestchat",
        b"",
    ];
    words == expected
}
