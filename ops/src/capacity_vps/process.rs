//! Synchronous bounded pipes and a reserved, unreaped process-group leader.

use super::{
    Failure, Result,
    control::{Fence, Owner, now},
};
use nix::{
    sys::{
        signal::{Signal, killpg},
        wait::{Id, WaitPidFlag, WaitStatus, waitid, waitpid},
    },
    unistd::Pid,
};
use rustix::fs::OFlags;
use std::{
    collections::BTreeMap,
    ffi::CString,
    fs::File,
    io::Read,
    os::{
        fd::{AsFd, AsRawFd},
        unix::ffi::OsStrExt,
    },
    path::Path,
    time::Duration,
};

const SECOND: u64 = 1_000_000_000;

fn nonblocking(fd: impl AsFd) -> Result<()> {
    let flags = rustix::fs::fcntl_getfl(&fd).map_err(|_| Failure::Io)?;
    rustix::fs::fcntl_setfl(fd, flags | OFlags::NONBLOCK).map_err(|_| Failure::Io)
}

pub(super) struct Input {
    stdin: std::io::Stdin,
    previous: OFlags,
    restored: bool,
}
impl Input {
    pub fn new() -> Result<Self> {
        let stdin = std::io::stdin();
        let previous = rustix::fs::fcntl_getfl(&stdin).map_err(|_| Failure::Io)?;
        let mut value = Self {
            stdin,
            previous,
            restored: false,
        };
        if let Err(error) = nonblocking(&value.stdin) {
            let _ = value.restore();
            return Err(error);
        }
        Ok(value)
    }
    pub fn exact(&mut self, count: usize, owner: &mut Owner, end: u64) -> Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(count.min(65536));
        let mut buffer = [0u8; 65536];
        while bytes.len() < count {
            owner.check(false)?;
            if now()? >= end {
                return Err(Failure::Deadline);
            }
            let maximum = buffer.len().min(count - bytes.len());
            match rustix::io::read(&self.stdin, &mut buffer[..maximum]) {
                Ok(0) => return Err(Failure::Framing),
                Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                Err(rustix::io::Errno::AGAIN) => std::thread::sleep(Duration::from_millis(10)),
                Err(rustix::io::Errno::INTR) => {}
                Err(_) => return Err(Failure::Io),
            }
        }
        Ok(bytes)
    }
    pub fn eof(&mut self, owner: &mut Owner, end: u64) -> Result<()> {
        let mut one = [0u8; 1];
        loop {
            owner.check(false)?;
            if now()? >= end {
                return Err(Failure::Deadline);
            }
            match rustix::io::read(&self.stdin, &mut one) {
                Ok(0) => return Ok(()),
                Ok(_) => return Err(Failure::Framing),
                Err(rustix::io::Errno::AGAIN) => std::thread::sleep(Duration::from_millis(10)),
                Err(rustix::io::Errno::INTR) => {}
                Err(_) => return Err(Failure::Io),
            }
        }
    }
    fn restore(&mut self) -> Result<()> {
        if self.restored {
            return Ok(());
        }
        self.restored = true;
        rustix::fs::fcntl_setfl(&self.stdin, self.previous).map_err(|_| Failure::Io)
    }
    pub fn finish(mut self) -> Result<()> {
        self.restore()
    }
}
impl Drop for Input {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

pub(super) struct Output {
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub combined: Vec<u8>,
}
struct Group {
    pid: Pid,
    reserved: bool,
    reap_pending: bool,
    status: Option<i32>,
    stdout: Option<File>,
    stderr: Option<File>,
}
#[cfg(test)]
#[derive(Default)]
struct CloseProbe {
    fail_next: bool,
    calls: usize,
    retain_identity: bool,
    identity: Option<std::os::fd::OwnedFd>,
}
#[cfg(test)]
thread_local! {
    static CLOSE_PROBE:std::cell::RefCell<CloseProbe>=const {
        std::cell::RefCell::new(CloseProbe {
            fail_next:false,calls:0,retain_identity:false,identity:None
        })
    };
}
fn close_pipe(file: File) -> Result<()> {
    let result = nix::unistd::close(file).map_err(|_| Failure::Close);
    #[cfg(test)]
    {
        CLOSE_PROBE.with(|probe| {
            let mut probe = probe.borrow_mut();
            probe.calls += 1;
            if std::mem::take(&mut probe.fail_next) {
                result.and(Err(Failure::Close))
            } else {
                result
            }
        })
    }
    #[cfg(not(test))]
    {
        result
    }
}
fn remember(primary: &mut Option<Failure>, result: Result<()>) {
    if let Err(error) = result {
        primary.get_or_insert(error);
    }
}
impl Group {
    fn spawn(
        program: &Path,
        arguments: &[String],
        environment: &BTreeMap<String, String>,
        owner: &mut Owner,
        grace_seconds: u64,
        observed_status: &mut Option<i32>,
    ) -> Result<Self> {
        *observed_status = None;
        use nix::spawn::{PosixSpawnAttr, PosixSpawnFileActions, PosixSpawnFlags};
        // nix owns the initialized opaque libc objects. C strings and borrowed
        // signal/action storage outlive the synchronous spawn call; no callback
        // crosses this boundary. nix's Drop destroys both opaque objects while
        // discarding libc destroy statuses and offers no checked consuming API.
        // That limitation is not a checked native-object destruction claim;
        // every actual pipe/null FD has a separate explicit close boundary.
        let mut files: Vec<Option<File>> = Vec::new();
        let spawned = (|| {
            files.push(Some(File::open("/dev/null").map_err(|_| Failure::Io)?));
            for _ in 0..2 {
                let (read, write) =
                    nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC).map_err(|_| Failure::Io)?;
                files.push(Some(File::from(read)));
                files.push(Some(File::from(write)));
            }
            if files.iter().flatten().any(|v| v.as_raw_fd() < 3) {
                return Err(Failure::Unproven);
            }
            let mut actions = PosixSpawnFileActions::init().map_err(|_| Failure::Io)?;
            for (old, new) in [(0, 0), (2, 1), (4, 2)] {
                actions
                    .add_dup2(files[old].as_ref().unwrap().as_raw_fd(), new)
                    .map_err(|_| Failure::Io)?;
            }
            // All five originals are CLOEXEC; duplicated stdio is the only
            // selected pipe authority inherited by the new program.
            let mut attributes = PosixSpawnAttr::init().map_err(|_| Failure::Io)?;
            attributes
                .set_pgroup(Pid::from_raw(0))
                .map_err(|_| Failure::Io)?;
            attributes
                .set_sigmask(&owner.child_mask()?)
                .map_err(|_| Failure::Io)?;
            let mut defaults = nix::sys::signal::SigSet::empty();
            defaults.add(Signal::SIGINT);
            defaults.add(Signal::SIGTERM);
            attributes
                .set_sigdefault(&defaults)
                .map_err(|_| Failure::Io)?;
            attributes
                .set_flags(
                    PosixSpawnFlags::POSIX_SPAWN_SETPGROUP
                        | PosixSpawnFlags::POSIX_SPAWN_SETSIGMASK
                        | PosixSpawnFlags::POSIX_SPAWN_SETSIGDEF,
                )
                .map_err(|_| Failure::Io)?;
            let mut argv =
                vec![CString::new(program.as_os_str().as_bytes()).map_err(|_| Failure::Arguments)?];
            for argument in arguments {
                argv.push(CString::new(argument.as_bytes()).map_err(|_| Failure::Arguments)?);
            }
            let mut env = Vec::new();
            for (key, value) in environment {
                env.push(CString::new(format!("{key}={value}")).map_err(|_| Failure::Arguments)?);
            }
            nix::spawn::posix_spawn(program, &actions, &attributes, &argv, &env)
                .map_err(|_| Failure::Command)
        })();
        let mut group = spawned.as_ref().ok().map(|pid| Self {
            pid: *pid,
            reserved: true,
            reap_pending: true,
            status: None,
            stdout: files[1].take(),
            stderr: files[3].take(),
        });
        #[cfg(test)]
        if let Ok(pid) = &spawned {
            CLOSE_PROBE.with(|probe| {
                let mut probe = probe.borrow_mut();
                if probe.retain_identity {
                    // Linux5.4+ test observer only, acquired while PID custody
                    // is reserved. Never re-use a disarmed numeric PID to test
                    // the consuming-reap boundary.
                    probe.identity = rustix::process::Pid::from_raw(pid.as_raw()).and_then(|pid| {
                        rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty()).ok()
                    });
                }
            });
        }
        let mut closed = None;
        for file in files.into_iter().flatten() {
            remember(&mut closed, close_pipe(file));
        }
        match spawned {
            Err(error) => Err(error),
            Ok(_) => {
                let mut group = group.take().unwrap();
                if let Some(error) = closed {
                    owner.latch(error);
                    let _ = group.finish(owner, grace_seconds);
                    *observed_status = group.status;
                    return Err(error);
                }
                Ok(group)
            }
        }
    }
    fn exited(&self) -> Result<bool> {
        if !self.reserved {
            return Err(Failure::Unproven);
        }
        match waitid(
            Id::Pid(self.pid),
            WaitPidFlag::WEXITED | WaitPidFlag::WNOWAIT | WaitPidFlag::WNOHANG,
        ) {
            Ok(WaitStatus::StillAlive) => Ok(false),
            Ok(WaitStatus::Exited(..) | WaitStatus::Signaled(..)) => Ok(true),
            Ok(_) => Err(Failure::Unproven),
            Err(nix::errno::Errno::EINTR) => Ok(false),
            Err(_) => Err(Failure::Io),
        }
    }
    fn signal(&self, signal: Signal) -> Result<()> {
        if !self.reserved {
            return Err(Failure::Unproven);
        }
        match killpg(self.pid, signal) {
            Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
            Err(_) => Err(Failure::Io),
        }
    }
    fn finish(&mut self, owner: &mut Owner, grace_seconds: u64) -> Result<i32> {
        let mut primary = None;
        remember(&mut primary, self.signal(Signal::SIGTERM));
        if let Some(error) = primary {
            owner.latch(error);
        }
        let grace = now().map(|v| {
            v.saturating_add(grace_seconds.saturating_mul(SECOND))
                .min(owner.end())
        });
        match grace {
            Ok(end) => loop {
                match now() {
                    Ok(v) if v < end => {}
                    Ok(_) => break,
                    Err(e) => {
                        primary.get_or_insert(e);
                        break;
                    }
                }
                match self.exited() {
                    Ok(true) => break,
                    Ok(false) => {}
                    Err(e) => {
                        primary.get_or_insert(e);
                        break;
                    }
                }
                let _ = owner.cleanup_time();
                std::thread::sleep(Duration::from_millis(10));
            },
            Err(e) => {
                primary.get_or_insert(e);
            }
        }
        if let Some(error) = primary {
            owner.latch(error);
        }
        remember(&mut primary, self.signal(Signal::SIGKILL));
        if let Some(error) = primary {
            owner.latch(error);
        }
        let mut settled = false;
        let end = now().map(|v| v.saturating_add(20 * SECOND).min(owner.end()));
        match end {
            Ok(end) => loop {
                match now() {
                    Ok(v) if v < end => {}
                    Ok(_) => break,
                    Err(e) => {
                        primary.get_or_insert(e);
                        break;
                    }
                }
                match self
                    .exited()
                    .and_then(|exited| Ok(exited && !live_group(self.pid, owner)?))
                {
                    Ok(true) => {
                        settled = true;
                        break;
                    }
                    Ok(false) => {}
                    Err(e) => {
                        primary.get_or_insert(e);
                        break;
                    }
                }
                let _ = owner.cleanup_time();
                std::thread::sleep(Duration::from_millis(10));
            },
            Err(e) => {
                primary.get_or_insert(e);
            }
        }
        if !settled {
            primary.get_or_insert(Failure::Unproven);
        }
        if let Some(error) = primary {
            owner.latch(error);
        }
        let status = match self.exited() {
            Ok(true) => {
                // Disarm BEFORE consuming wait; no later signal may hit reused PID.
                self.reserved = false;
                loop {
                    match waitpid(self.pid, Some(WaitPidFlag::WNOHANG)) {
                        Ok(WaitStatus::Exited(_, code)) => {
                            self.reap_pending = false;
                            self.status = Some(code);
                            break Ok(code);
                        }
                        // Keep Python Popen's signed child returncode in private
                        // evidence. Public owner interruption codes are separate.
                        Ok(WaitStatus::Signaled(_, signal, _)) => {
                            self.reap_pending = false;
                            self.status = Some(-(signal as i32));
                            break Ok(-(signal as i32));
                        }
                        Err(nix::errno::Errno::EINTR) => {
                            if let Err(error) = owner.cleanup_time() {
                                break Err(error);
                            }
                        }
                        _ => break Err(Failure::Unproven),
                    }
                }
            }
            Ok(false) => Err(Failure::Unproven),
            Err(e) => Err(e),
        };
        for file in [self.stdout.take(), self.stderr.take()]
            .into_iter()
            .flatten()
        {
            remember(&mut primary, close_pipe(file));
        }
        if let Some(error) = primary {
            owner.latch(error);
            Err(error)
        } else {
            status
        }
    }
}
impl Drop for Group {
    fn drop(&mut self) {
        if self.reserved {
            let _ = self.signal(Signal::SIGKILL);
            self.reserved = false;
        }
        if self.reap_pending {
            let _ = waitpid(self.pid, Some(WaitPidFlag::WNOHANG));
        }
    }
}

#[cfg(target_os = "linux")]
fn live_group(group: Pid, owner: &mut Owner) -> Result<bool> {
    let directory = File::open("/proc").map_err(|_| Failure::Io)?;
    let body = (|| {
        let mut buffer = [std::mem::MaybeUninit::uninit(); 8192];
        let mut cursor = rustix::fs::RawDir::new(&directory, &mut buffer);
        let mut count = 0usize;
        while let Some(entry) = cursor.next() {
            owner.cleanup_time()?;
            count += 1;
            if count > 100_000 {
                return Err(Failure::Bounds);
            }
            let entry = entry.map_err(|_| Failure::Io)?;
            let name = entry.file_name().to_bytes();
            if name.is_empty() || !name.iter().all(u8::is_ascii_digit) {
                continue;
            }
            let name = std::str::from_utf8(name).map_err(|_| Failure::Io)?;
            let mut file = match File::open(format!("/proc/{name}/stat")) {
                Ok(v) => v,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(Failure::Io),
            };
            let mut bytes = Vec::new();
            let read = file.by_ref().take(65537).read_to_end(&mut bytes);
            let closed = nix::unistd::close(file).map_err(|_| Failure::Close);
            read.map_err(|_| Failure::Io)?;
            closed?;
            if bytes.len() > 65536 {
                return Err(Failure::Bounds);
            }
            let text = std::str::from_utf8(&bytes).map_err(|_| Failure::Io)?;
            let rest = text.rsplit_once(')').ok_or(Failure::Unproven)?.1;
            let mut values = rest.split_whitespace();
            let state = values.next().ok_or(Failure::Unproven)?;
            let _parent = values.next().ok_or(Failure::Unproven)?;
            let pgid = values
                .next()
                .ok_or(Failure::Unproven)?
                .parse::<i32>()
                .map_err(|_| Failure::Unproven)?;
            if pgid == group.as_raw()
                && name != group.as_raw().to_string()
                && !matches!(state, "Z" | "X")
            {
                return Ok(true);
            }
        }
        Ok(false)
    })();
    let closed = nix::unistd::close(directory).map_err(|_| Failure::Close);
    let value = body?;
    closed?;
    Ok(value)
}
#[cfg(not(target_os = "linux"))]
fn live_group(_: Pid, _: &mut Owner) -> Result<bool> {
    Err(Failure::Platform)
}

/// Arguments originate only in the fixed host adapters, never serialized paths.
pub(super) struct Invocation<'a> {
    pub program: &'a Path,
    pub arguments: &'a [String],
    pub environment: &'a BTreeMap<String, String>,
    pub seconds: u64,
    pub maximum: usize,
    pub cleanup: bool,
}

pub(super) struct Runner {
    ordinary: u64,
    cleanup: u64,
    last: Option<Output>,
    #[cfg(test)]
    fixture: Option<Fixture>,
}
#[cfg(test)]
type Fixture =
    Box<dyn FnMut(&Path, &[String], &BTreeMap<String, String>, u64, bool) -> Result<Output>>;
impl Runner {
    #[cfg(test)]
    pub fn fixture(fixture: Fixture) -> Self {
        Self {
            ordinary: 0,
            cleanup: 0,
            last: None,
            fixture: Some(fixture),
        }
    }
    pub fn new() -> Self {
        Self {
            ordinary: 0,
            cleanup: 0,
            last: None,
            #[cfg(test)]
            fixture: None,
        }
    }
    pub fn take_capture(&mut self) -> Option<Output> {
        self.last.take()
    }
    pub fn capacity(
        &mut self,
        program: &Path,
        arguments: &[String],
        environment: &BTreeMap<String, String>,
        seconds: u64,
        owner: &mut Owner,
    ) -> Result<Output> {
        self.run_owned(
            Invocation {
                program,
                arguments,
                environment,
                seconds,
                maximum: 8 * 1024 * 1024,
                cleanup: false,
            },
            600,
            owner,
        )
    }
    pub fn run(&mut self, invocation: Invocation<'_>, owner: &mut Owner) -> Result<Output> {
        self.run_owned(invocation, 5, owner)
    }
    fn run_owned(
        &mut self,
        invocation: Invocation<'_>,
        grace_seconds: u64,
        owner: &mut Owner,
    ) -> Result<Output> {
        let Invocation {
            program,
            arguments,
            environment,
            seconds,
            maximum,
            cleanup,
        } = invocation;
        self.last = None;
        if cleanup {
            owner.cleanup_time()?;
        } else {
            owner.check(false)?;
        }
        #[cfg(test)]
        if let Some(fixture) = &mut self.fixture {
            let result = fixture(program, arguments, environment, seconds, cleanup);
            if let Err(error) = &result {
                owner.latch(*error);
            }
            let checked = if cleanup {
                owner.cleanup_time()
            } else {
                owner.check(false)
            };
            return result.and_then(|output| {
                checked?;
                Ok(output)
            });
        }
        let mut observed_status = None;
        let mut group = match Group::spawn(
            program,
            arguments,
            environment,
            owner,
            grace_seconds,
            &mut observed_status,
        ) {
            Ok(group) => group,
            Err(error) => {
                // Parent-pipe refusal after real spawn can still have a known
                // consuming reap. Empty bytes are partial, never complete I/O.
                self.last = Some(Output {
                    status: observed_status,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    combined: Vec::new(),
                });
                owner.latch(error);
                return Err(error);
            }
        };
        let mut output = Output {
            status: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            combined: Vec::new(),
        };
        let work = (|| {
            let stdout = group.stdout.as_ref().ok_or(Failure::Unproven)?;
            let stderr = group.stderr.as_ref().ok_or(Failure::Unproven)?;
            nonblocking(stdout)?;
            nonblocking(stderr)?;
            let end = now()?
                .checked_add(seconds.checked_mul(SECOND).ok_or(Failure::Bounds)?)
                .ok_or(Failure::Bounds)?
                .min(if cleanup {
                    owner.end()
                } else {
                    owner.active_end()
                });
            let (mut out_eof, mut err_eof) = (false, false);
            let mut buffer = [0u8; 65536];
            loop {
                if cleanup {
                    owner.cleanup_time()?;
                } else {
                    owner.check(false)?;
                }
                if now()? >= end {
                    return Err(Failure::Deadline);
                }
                for (fd, destination, eof, limit) in [
                    (stdout.as_fd(), &mut output.stdout, &mut out_eof, maximum),
                    (
                        stderr.as_fd(),
                        &mut output.stderr,
                        &mut err_eof,
                        2 * 1024 * 1024,
                    ),
                ] {
                    if *eof {
                        continue;
                    }
                    match rustix::io::read(fd, &mut buffer) {
                        Ok(0) => *eof = true,
                        Ok(n) => {
                            if n > limit.saturating_sub(destination.len()) {
                                return Err(Failure::Bounds);
                            }
                            let total = if cleanup {
                                &mut self.cleanup
                            } else {
                                &mut self.ordinary
                            };
                            let bound = if cleanup {
                                8 * 1024 * 1024
                            } else {
                                248 * 1024 * 1024
                            };
                            *total = total.checked_add(n as u64).ok_or(Failure::Bounds)?;
                            if *total > bound {
                                return Err(Failure::Bounds);
                            }
                            destination.extend_from_slice(&buffer[..n]);
                            output.combined.extend_from_slice(&buffer[..n]);
                        }
                        Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => {}
                        Err(_) => return Err(Failure::Io),
                    }
                }
                if out_eof && err_eof && group.exited()? {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        })();
        if let Err(error) = work.as_ref() {
            owner.latch(*error);
        }
        let finish = group.finish(owner, grace_seconds);
        output.status = group.status;
        if let Err(error) = &finish {
            owner.latch(*error);
        }
        match (work, finish) {
            (Ok(()), Ok(status)) => {
                output.status = Some(status);
                Ok(output)
            }
            (Err(error), _) | (_, Err(error)) => {
                self.last = Some(output);
                Err(error)
            }
        }
    }
}

pub(super) fn environment() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("PATH".into(), "/usr/sbin:/usr/bin:/sbin:/bin".into()),
        ("LC_ALL".into(), "C".into()),
        ("HOME".into(), "/root".into()),
    ])
}

enum WriteClock<'a> {
    Live(&'a mut Owner, u64),
    Finished(&'a mut Fence),
}
impl WriteClock<'_> {
    fn check(&mut self) -> Result<()> {
        match self {
            Self::Live(owner, end) => {
                owner.check(false)?;
                if now()? >= *end {
                    Err(Failure::Deadline)
                } else {
                    Ok(())
                }
            }
            Self::Finished(fence) => fence.check(),
        }
    }
}
fn write_record(bytes: &[u8], mut clock: WriteClock<'_>) -> Result<()> {
    let stdout = std::io::stdout();
    let flags = rustix::fs::fcntl_getfl(&stdout).map_err(|_| Failure::Io)?;
    let set = rustix::fs::fcntl_setfl(&stdout, flags | OFlags::NONBLOCK).map_err(|_| Failure::Io);
    let body = (|| {
        set?;
        let mut count = 0;
        while count < bytes.len() {
            clock.check()?;
            match rustix::io::write(&stdout, &bytes[count..]) {
                Ok(0) => return Err(Failure::Io),
                Ok(n) => count += n,
                Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(_) => return Err(Failure::Io),
            }
        }
        clock.check()
    })();
    let restored = rustix::fs::fcntl_setfl(&stdout, flags).map_err(|_| Failure::Io);
    body.and(restored)
}
pub(super) fn ready(bytes: &[u8], owner: &mut Owner) -> Result<()> {
    owner.ready()?;
    let end = owner.fixed_end(120)?;
    write_record(bytes, WriteClock::Live(owner, end))
}
pub(super) fn emit(bytes: &[u8], fence: &mut Fence) -> Result<()> {
    write_record(bytes, WriteClock::Finished(fence))?;
    fence.check()
}
pub(super) fn failure(error: Failure) {
    let stderr = std::io::stderr();
    if let Ok(flags) = rustix::fs::fcntl_getfl(&stderr) {
        if rustix::fs::fcntl_setfl(&stderr, flags | OFlags::NONBLOCK).is_ok() {
            let bytes = format!("{}\n", error.code());
            let _ = rustix::io::write(&stderr, bytes.as_bytes());
        }
        let _ = rustix::fs::fcntl_setfl(&stderr, flags);
    }
}

#[cfg(test)]
#[path = "process_tests.rs"]
mod tests;
