//! Bounded retained-tool children, effect adoption and exact-group finalization.

use super::{
    Error, PRIVATE, ROOT, Result,
    control::Control,
    files::{self, Original},
};
use crate::owned_process::{Group, exited_now, finish_before, reserved};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::{
    collections::BTreeMap,
    fs::{self, File, Metadata, OpenOptions},
    io::Write,
    os::{
        fd::OwnedFd,
        unix::{
            fs::{MetadataExt, OpenOptionsExt},
            process::{CommandExt, ExitStatusExt},
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const MAX: usize = 2 * 1024 * 1024;
fn retained_executable(original: &Original) -> Result<PathBuf> {
    #[cfg(target_os = "linux")]
    let path = PathBuf::from(format!(
        "/proc/{}/fd/{}",
        std::process::id(),
        original.file()?.as_raw_fd()
    ));
    #[cfg(not(target_os = "linux"))]
    let path = original.path.clone();
    Ok(path)
}
pub(super) struct Output {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
pub(super) struct OutputChunk {
    pub stdout: Vec<u8>,
    pub stdout_eof: bool,
    pub stderr_eof: bool,
}
struct Pipe {
    file: Option<File>,
    flags: Option<rustix::fs::OFlags>,
    eof: bool,
}
impl Pipe {
    fn open(file: File) -> Self {
        Self {
            file: Some(file),
            flags: None,
            eof: false,
        }
    }
    fn configure(&mut self) -> Result<()> {
        let file = self.file.as_ref().ok_or(Error::Close)?;
        let flags = rustix::fs::fcntl_getfl(file).map_err(|_| Error::Io)?;
        self.flags = Some(flags);
        rustix::fs::fcntl_setfl(file, flags | rustix::fs::OFlags::NONBLOCK).map_err(|_| Error::Io)
    }
    fn read(&mut self, maximum: usize) -> Result<Vec<u8>> {
        let mut bytes = [0u8; 65536];
        let size = maximum.min(bytes.len());
        match rustix::io::read(self.file.as_ref().ok_or(Error::Close)?, &mut bytes[..size]) {
            Ok(0) => {
                self.eof = true;
                Ok(Vec::new())
            }
            Ok(n) => Ok(bytes[..n].to_vec()),
            Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => Ok(Vec::new()),
            Err(_) => Err(Error::Io),
        }
    }
    fn close(&mut self, control: &mut Control) -> Result<()> {
        let mut first = None;
        if let Some(file) = self.file.take() {
            if let Some(flags) = self.flags.take()
                && rustix::fs::fcntl_setfl(&file, flags).is_err()
            {
                first = Some(Error::Close);
                control.cleanup_failed = true;
            }
            if let Err(error) = files::close(file, control) {
                first.get_or_insert(error);
            }
        }
        first.map_or(Ok(()), Err)
    }
}

pub(super) struct Running {
    group: Option<Group>,
    pipes: [Option<Pipe>; 2],
    logs: [Option<File>; 2],
    output: [Vec<u8>; 2],
    end: Instant,
    first: Option<Error>,
    status: Option<i32>,
    closed: bool,
    retired: bool,
    reaped: bool,
    joined: bool,
    limits: [usize; 2],
    grace: Duration,
}
impl Running {
    pub fn pid(&self) -> rustix::process::Pid {
        self.group
            .as_ref()
            .expect("registered child retains its group until finish")
            .pid()
    }
    pub fn running(&self) -> Result<bool> {
        if self.closed {
            return Ok(false);
        }
        exited_now(self.pid()).map(|v| !v).map_err(|_| Error::Close)
    }
    pub fn status(&self) -> Option<i32> {
        self.status
    }
    pub fn set_retirement_grace(&mut self, grace: Duration) -> Result<()> {
        if grace.is_zero() || grace > Duration::from_secs(5) {
            return Err(Error::Input);
        }
        self.grace = grace;
        Ok(())
    }
    pub fn set_limits(&mut self, stdout: usize, stderr: usize) -> Result<()> {
        if stdout == 0 || stderr == 0 || stdout > MAX || stderr > MAX {
            return Err(Error::Input);
        }
        self.limits = [stdout, stderr];
        Ok(())
    }
    pub fn snapshot(&self) -> (&[u8], &[u8]) {
        (&self.output[0], &self.output[1])
    }
    pub fn pump(&mut self, control: &mut Control) -> Result<OutputChunk> {
        control.check()?;
        if Instant::now() >= self.end {
            return Err(Error::Deadline);
        }
        let mut delta = [Vec::new(), Vec::new()];
        for (index, bytes) in delta.iter_mut().enumerate() {
            let pipe = self.pipes[index].as_mut().ok_or(Error::Close)?;
            if !pipe.eof {
                *bytes =
                    pipe.read(self.limits[index].saturating_sub(self.output[index].len()) + 1)?;
                if self.output[index].len().saturating_add(bytes.len()) > self.limits[index] {
                    return Err(Error::Bounds);
                }
                self.output[index].extend_from_slice(bytes);
                if !bytes.is_empty() {
                    self.logs[index]
                        .as_mut()
                        .ok_or(Error::Close)?
                        .write_all(bytes)
                        .map_err(|_| Error::Io)?;
                }
                control.check()?;
            }
        }
        Ok(OutputChunk {
            stdout: std::mem::take(&mut delta[0]),
            stdout_eof: self.pipes[0].as_ref().is_some_and(|p| p.eof),
            stderr_eof: self.pipes[1].as_ref().is_some_and(|p| p.eof),
        })
    }
    fn complete(&self) -> Result<bool> {
        Ok(self.pipes.iter().all(|p| p.as_ref().is_some_and(|p| p.eof)) && !self.running()?)
    }
    pub fn signal(&mut self, control: &mut Control, signal: rustix::process::Signal) -> Result<()> {
        control.check()?;
        if !reserved(self.pid()) {
            return Err(Error::Close);
        }
        match rustix::process::kill_process_group(self.pid(), signal) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => control.check(),
            Err(_) => Err(Error::Close),
        }
    }
    pub async fn wait(&mut self, control: &mut Control, cap: Duration) -> Result<Output> {
        let end = (Instant::now() + cap)
            .min(self.end)
            .min(control.phase_end());
        let result = async {
            loop {
                self.pump(control)?;
                if self.complete()? {
                    break;
                }
                if Instant::now() >= end {
                    return Err(Error::Deadline);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            self.first.get_or_insert(error);
            control.record(error);
        }
        self.finish(control).await
    }
    async fn retire(&mut self, control: &mut Control) {
        if self.joined {
            return;
        }
        self.joined = true;
        if let Some(group) = &mut self.group {
            let pid = group.pid();
            if !exited_now(pid).unwrap_or(false) {
                match rustix::process::kill_process_group(pid, rustix::process::Signal::TERM) {
                    Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                    Err(_) => {
                        self.first.get_or_insert(Error::Close);
                    }
                }
                let end = (Instant::now() + self.grace).min(control.phase_end());
                while Instant::now() < end && !exited_now(pid).unwrap_or(true) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
            self.retired = finish_before(pid, control.phase_end()).await;
            if !self.retired {
                self.first.get_or_insert(Error::Close);
                control.cleanup_failed = true;
            }
            match group.reap().await {
                Ok(status) => {
                    self.reaped = true;
                    self.status = status.code().or_else(|| status.signal().map(|v| -v));
                }
                Err(_) => {
                    self.first.get_or_insert(Error::Close);
                    control.cleanup_failed = true;
                }
            }
        }
    }
    async fn drain(&mut self, control: &mut Control) {
        // Read known remaining output without admitting new work after termination.
        for index in 0..2 {
            if let Some(pipe) = &mut self.pipes[index] {
                let end = (Instant::now() + Duration::from_secs(1)).min(control.phase_end());
                while !pipe.eof && Instant::now() < end {
                    match pipe.read(self.limits[index].saturating_sub(self.output[index].len()) + 1)
                    {
                        Ok(bytes) => {
                            if self.output[index].len().saturating_add(bytes.len())
                                > self.limits[index]
                            {
                                self.first.get_or_insert(Error::Bounds);
                                break;
                            }
                            self.output[index].extend_from_slice(&bytes);
                            if let Some(log) = &mut self.logs[index]
                                && log.write_all(&bytes).is_err()
                            {
                                self.first.get_or_insert(Error::Io);
                            }
                            if bytes.is_empty() && !pipe.eof {
                                tokio::time::sleep(Duration::from_millis(5)).await;
                            }
                        }
                        Err(error) => {
                            self.first.get_or_insert(error);
                            break;
                        }
                    }
                }
                if !pipe.eof {
                    self.first.get_or_insert(Error::Close);
                    control.cleanup_failed = true;
                }
            }
        }
    }
    async fn settle(&mut self, control: &mut Control) -> Result<()> {
        if self.closed {
            return self.first.map_or(Ok(()), Err);
        }
        self.closed = true;
        self.retire(control).await;
        self.drain(control).await;
        for index in 0..2 {
            if let Some(pipe) = &mut self.pipes[index]
                && let Err(error) = pipe.close(control)
            {
                self.first.get_or_insert(error);
            }
            if let Some(mut log) = self.logs[index].take() {
                if log.flush().is_err() {
                    self.first.get_or_insert(Error::Io);
                }
                if let Err(error) = files::close(log, control) {
                    self.first.get_or_insert(error);
                }
            }
        }
        if self.group.is_some() && (!self.retired || !self.reaped || self.status.is_none()) {
            self.first.get_or_insert(Error::Close);
        }
        if let Err(error) = control.check() {
            self.first.get_or_insert(error);
        }
        if let Some(error) = self.first {
            control.record(error);
            return Err(error);
        }
        Ok(())
    }
    pub async fn finish(&mut self, control: &mut Control) -> Result<Output> {
        self.settle(control).await?;
        Ok(Output {
            status: self.status.ok_or(Error::Close)?,
            stdout: self.output[0].clone(),
            stderr: self.output[1].clone(),
        })
    }
    async fn finish_adopting<F>(&mut self, control: &mut Control, adopt: F) -> Result<Output>
    where
        F: FnOnce(&[u8]) -> Result<()>,
    {
        self.retire(control).await;
        self.drain(control).await;
        if self.status == Some(0)
            && self.pipes.iter().all(|p| p.as_ref().is_some_and(|p| p.eof))
            && let Err(error) = adopt(&self.output[0])
        {
            self.first.get_or_insert(error);
            control.record(error);
        }
        self.finish(control).await
    }
}

struct Tool {
    original: Original,
    alias: PathBuf,
    stamp: Metadata,
}
pub(super) type Monitor = Box<dyn FnMut(&mut Control) -> Result<()> + Send>;
#[derive(Clone, Copy)]
enum Actor {
    Events,
    Capture,
    Native,
    HostNative,
}
impl Actor {
    fn parse(role: &str, tool: Option<&String>) -> Result<Self> {
        match (role, tool.map(String::as_str)) {
            ("daemon-events", Some("docker")) => Ok(Self::Events),
            ("wire-capture", Some("tcpdump")) => Ok(Self::Capture),
            ("native", Some("ops")) => Ok(Self::Native),
            ("host-native", Some("ops")) => Ok(Self::HostNative),
            _ => Err(Error::Input),
        }
    }
    fn index(self) -> usize {
        match self {
            Self::Events => 0,
            Self::Capture => 1,
            Self::Native => 2,
            Self::HostNative => 3,
        }
    }
}
pub(super) struct Commands {
    tools: BTreeMap<String, Tool>,
    count: u16,
    ordinary: u16,
    actors: [bool; 4],
    ordinary_limit: u16,
    failed: Vec<Running>,
    monitor: Option<Monitor>,
}
impl Commands {
    pub fn new() -> Self {
        Self {
            tools: BTreeMap::new(),
            count: 0,
            ordinary: 0,
            actors: [false; 4],
            ordinary_limit: 128,
            failed: Vec::new(),
            monitor: None,
        }
    }
    pub fn new_host() -> Self {
        let mut value = Self::new();
        value.ordinary_limit = 256;
        value
    }
    pub fn admit(&mut self, role: &str, path: &Path, control: &mut Control) -> Result<()> {
        control.check()?;
        if self.tools.contains_key(role) {
            return Err(Error::Refused);
        }
        let alias = path.to_path_buf();
        let stamp = fs::symlink_metadata(path).map_err(|_| Error::Io)?;
        let target = fs::canonicalize(path).map_err(|_| Error::Io)?;
        let mode = fs::metadata(&target).map_err(|_| Error::Io)?.mode() & 0o7777;
        if mode & 0o111 == 0 || mode & 0o022 != 0 {
            return Err(Error::Refused);
        }
        let original = if role == "ops" {
            Original::open(&target, 256 * 1024 * 1024, 0, 0o755, control)?
        } else {
            Original::system_tool(&target, control)?
        };
        self.tools.insert(
            role.into(),
            Tool {
                original,
                alias,
                stamp,
            },
        );
        Ok(())
    }
    pub fn admit_lookup(&mut self, role: &str, control: &mut Control) -> Result<()> {
        for directory in ["/usr/sbin", "/usr/bin", "/sbin", "/bin"] {
            let path = Path::new(directory).join(role);
            if path.is_file() {
                return self.admit(role, &path, control);
            }
        }
        Err(Error::Refused)
    }
    pub fn set_monitor(&mut self, monitor: Option<Monitor>) {
        self.monitor = monitor;
    }
    pub fn monitor(&mut self, control: &mut Control) -> Result<()> {
        if let Some(monitor) = &mut self.monitor {
            monitor(control)?;
        }
        control.check()
    }
    pub fn checksum(&self, role: &str) -> Result<&str> {
        self.tools
            .get(role)
            .map(|t| t.original.sha256.as_str())
            .ok_or(Error::Refused)
    }
    pub fn check(&self, control: &mut Control) -> Result<()> {
        for tool in self.tools.values() {
            control.check()?;
            let now = fs::symlink_metadata(&tool.alias).map_err(|_| Error::Changed)?;
            if (
                now.dev(),
                now.ino(),
                now.mode(),
                now.uid(),
                now.gid(),
                now.len(),
                now.mtime(),
                now.mtime_nsec(),
                now.ctime(),
                now.ctime_nsec(),
            ) != (
                tool.stamp.dev(),
                tool.stamp.ino(),
                tool.stamp.mode(),
                tool.stamp.uid(),
                tool.stamp.gid(),
                tool.stamp.len(),
                tool.stamp.mtime(),
                tool.stamp.mtime_nsec(),
                tool.stamp.ctime(),
                tool.stamp.ctime_nsec(),
            ) || fs::canonicalize(&tool.alias).map_err(|_| Error::Changed)? != tool.original.path
            {
                return Err(Error::Changed);
            }
            tool.original.check(control)?;
        }
        control.check()
    }
    fn account(&mut self, actor: Option<Actor>) -> Result<()> {
        if let Some(actor) = actor {
            if matches!(actor, Actor::HostNative) && self.ordinary_limit != 256 {
                return Err(Error::Input);
            }
            let used = &mut self.actors[actor.index()];
            if *used {
                return Err(Error::Input);
            }
            *used = true;
        } else {
            if self.ordinary >= self.ordinary_limit {
                return Err(Error::Input);
            }
            self.ordinary += 1;
        }
        self.count += 1;
        Ok(())
    }
    fn spawn_at(
        &mut self,
        control: &mut Control,
        role: &str,
        args: &[String],
        end: Instant,
        actor: Option<Actor>,
    ) -> Result<Running> {
        self.check(control)?;
        if args.is_empty()
            || role.is_empty()
            || role.len() > 48
            || !role
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(Error::Input);
        }
        self.account(actor)?;
        let mut running = Running {
            group: None,
            pipes: [None, None],
            logs: [None, None],
            output: [Vec::new(), Vec::new()],
            end,
            first: None,
            status: None,
            closed: false,
            retired: false,
            reaped: false,
            joined: false,
            limits: [MAX, MAX],
            grace: Duration::from_secs(5),
        };
        let result = (|| {
            for (index, suffix) in ["stdout", "stderr"].iter().enumerate() {
                running.logs[index] = Some(
                    OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .custom_flags(
                            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC).bits()
                                as i32,
                        )
                        .open(Path::new(PRIVATE).join(format!("{}-{role}.{suffix}", self.count)))
                        .map_err(|_| Error::Io)?,
                );
            }
            let tool = self.tools.get(&args[0]).ok_or(Error::Refused)?;
            // The retained parent owns this descriptor through interpreter startup and
            // finalization. A shebang interpreter cannot use a child CLOEXEC descriptor.
            let executable = retained_executable(&tool.original)?;
            let mut command = Command::new(executable);
            command
                .arg0(&tool.original.path)
                .args(&args[1..])
                .env_clear()
                .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
                .env("LC_ALL", "C")
                .env("HOME", ROOT)
                .current_dir(ROOT)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0);
            control.check()?;
            running.group = Some(Group::new(command.spawn().map_err(|_| Error::Io)?));
            let child = running.group.as_mut().ok_or(Error::Close)?.child();
            let files = [
                child.stdout.take().map(|p| File::from(OwnedFd::from(p))),
                child.stderr.take().map(|p| File::from(OwnedFd::from(p))),
            ];
            for (index, file) in files.into_iter().enumerate() {
                if let Some(file) = file {
                    running.pipes[index] = Some(Pipe::open(file));
                } else {
                    return Err(Error::Close);
                }
            }
            for pipe in running.pipes.iter_mut().flatten() {
                pipe.configure()?;
            }
            control.check()
        })();
        if let Err(error) = result {
            running.first = Some(error);
            control.record(error);
            self.failed.push(running);
            return Err(error);
        }
        Ok(running)
    }
    pub fn spawn(
        &mut self,
        control: &mut Control,
        role: &str,
        args: &[String],
        cap: Duration,
    ) -> Result<Running> {
        let actor = Actor::parse(role, args.first())?;
        self.spawn_at(
            control,
            role,
            args,
            (Instant::now() + cap).min(control.end()),
            Some(actor),
        )
    }
    async fn settle_failed_start<F>(
        &mut self,
        control: &mut Control,
        failed_before: usize,
        adopt: F,
    ) where
        F: FnOnce(&[u8]) -> Result<()>,
    {
        if self.failed.len() > failed_before
            && let Some(mut child) = self.failed.pop()
        {
            // The same callback must see a real raw-zero completed creation
            // even if startup's last flag or clock check failed after spawn.
            let _ = child.finish_adopting(control, adopt).await;
        }
    }
    async fn execute_adopt<F>(
        &mut self,
        control: &mut Control,
        role: &str,
        args: &[String],
        cap: Duration,
        accepted: &[i32],
        adopt: F,
    ) -> Result<Output>
    where
        F: FnOnce(&[u8]) -> Result<()>,
    {
        let end = (Instant::now() + cap).min(control.phase_end());
        let failed_before = self.failed.len();
        let mut child = match self.spawn_at(control, role, args, end, None) {
            Ok(child) => child,
            Err(error) => {
                self.settle_failed_start(control, failed_before, adopt)
                    .await;
                return Err(error);
            }
        };
        let body = async {
            loop {
                self.monitor(control)?;
                child.pump(control)?;
                if child.complete()? {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok(())
        }
        .await;
        if let Err(error) = body {
            child.first.get_or_insert(error);
            control.record(error);
        }
        // Adopt actual completed effects before any fallible sink close or post-hash.
        // The reservation remains held until settlement; output is complete only with EOF.
        let output = child.finish_adopting(control, adopt).await?;
        if !accepted.contains(&output.status) {
            return Err(Error::Command);
        }
        self.check(control)?;
        Ok(output)
    }
    pub async fn execute(
        &mut self,
        control: &mut Control,
        role: &str,
        args: &[String],
        cap: Duration,
        accepted: &[i32],
    ) -> Result<Output> {
        self.execute_adopt(control, role, args, cap, accepted, |_| Ok(()))
            .await
    }
    pub async fn call(
        &mut self,
        control: &mut Control,
        role: &str,
        args: &[String],
        cap: Duration,
    ) -> Result<Vec<u8>> {
        self.execute(control, role, args, cap, &[0])
            .await
            .map(|v| v.stdout)
    }
    pub async fn call_adopt<F>(
        &mut self,
        control: &mut Control,
        role: &str,
        args: &[String],
        cap: Duration,
        adopt: F,
    ) -> Result<Vec<u8>>
    where
        F: FnOnce(&[u8]) -> Result<()>,
    {
        self.execute_adopt(control, role, args, cap, &[0], adopt)
            .await
            .map(|v| v.stdout)
    }
    pub async fn finish(&mut self, control: &mut Control) -> Result<()> {
        self.monitor = None;
        let mut first = None;
        for running in &mut self.failed {
            if let Err(error) = running.finish(control).await {
                first.get_or_insert(error);
            }
        }
        for tool in self.tools.values_mut() {
            if let Err(error) = tool.original.finish(control) {
                first.get_or_insert(error);
            }
        }
        first.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "maintain-guest-process-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(fs::canonicalize(path).unwrap())
        }
        fn spawn(&self, mode: &str, readonly_log: bool) -> Running {
            let name = concat!(module_path!(), "::guest_core_inert_child");
            let name = name.split_once("::").unwrap().1;
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", name, "--nocapture", "--test-threads=1"])
                .env("SIMPLESTCHAT_MAINTAIN_GUEST_TEST_ACTOR", mode);
            self.spawn_command(command, readonly_log)
        }
        fn spawn_command(&self, mut command: Command, readonly_log: bool) -> Running {
            command
                .current_dir(&self.0)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0);
            let mut group = Group::new(command.spawn().unwrap());
            let output = File::from(OwnedFd::from(group.child().stdout.take().unwrap()));
            let error = File::from(OwnedFd::from(group.child().stderr.take().unwrap()));
            let log = self.0.join("stdout");
            fs::write(&log, b"").unwrap();
            let output_log = OpenOptions::new()
                .read(true)
                .write(!readonly_log)
                .open(log)
                .unwrap();
            let error_log = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(self.0.join("stderr"))
                .unwrap();
            let mut running = Running {
                group: Some(group),
                pipes: [Some(Pipe::open(output)), Some(Pipe::open(error))],
                logs: [Some(output_log), Some(error_log)],
                output: [Vec::new(), Vec::new()],
                end: Instant::now() + Duration::from_secs(10),
                first: None,
                status: None,
                closed: false,
                retired: false,
                reaped: false,
                joined: false,
                limits: [MAX, MAX],
                grace: Duration::from_secs(1),
            };
            for pipe in running.pipes.iter_mut().flatten() {
                pipe.configure().unwrap();
            }
            running
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn guest_core_inert_child() {
        let Ok(mode) = std::env::var("SIMPLESTCHAT_MAINTAIN_GUEST_TEST_ACTOR") else {
            return;
        };
        match mode.as_str() {
            "zero" | "nonzero" => {
                std::io::stdout()
                    .write_all(format!("\n{}\n", "a".repeat(64)).as_bytes())
                    .unwrap();
                std::io::stdout().flush().unwrap();
                std::process::exit(if mode == "zero" { 0 } else { 7 });
            }
            _ => panic!("fixed inert actor mode"),
        }
    }
    #[test]
    fn ordinary_command_129_is_refused_without_spending_actor_or_clock_authority() {
        let mut commands = Commands::new();
        let mut owner = Control::new(Instant::now()).unwrap();
        let original_end = owner.end();
        for _ in 0..128 {
            commands.account(None).unwrap();
        }
        let arguments = vec!["docker".into()];
        assert!(matches!(
            commands.spawn_at(&mut owner, "ordinary", &arguments, original_end, None),
            Err(Error::Input)
        ));
        assert_eq!(commands.count, 128);
        assert_eq!(commands.ordinary, 128);
        assert!(commands.failed.is_empty());
        assert!(commands.tools.is_empty());
        assert_eq!(owner.end(), original_end);
        assert_eq!(commands.actors, [false; 4]);
        for actor in [Actor::Events, Actor::Capture, Actor::Native] {
            commands.account(Some(actor)).unwrap();
            assert_eq!(commands.account(Some(actor)), Err(Error::Input));
        }
        assert_eq!(commands.count, 131);
        assert_eq!(commands.ordinary, 128);
        owner.begin_settling();
        owner.finish().unwrap();
    }
    #[test]
    fn host_second_native_actor_is_bounded_and_unavailable_to_original_purpose() {
        let mut original = Commands::new();
        assert_eq!(original.account(Some(Actor::HostNative)), Err(Error::Input));
        assert_eq!(original.count, 0);
        let mut host = Commands::new_host();
        for _ in 0..256 {
            host.account(None).unwrap();
        }
        assert_eq!(host.account(None), Err(Error::Input));
        for actor in [
            Actor::Events,
            Actor::Capture,
            Actor::Native,
            Actor::HostNative,
        ] {
            host.account(Some(actor)).unwrap();
            assert_eq!(host.account(Some(actor)), Err(Error::Input));
        }
        assert_eq!(host.count, 260);
        assert_eq!(host.ordinary, 256);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn raw_zero_creation_is_adopted_after_body_sink_failure_before_settlement() {
        let fixture = Fixture::new();
        let mut owner = Control::new(Instant::now()).unwrap();
        let mut running = fixture.spawn("zero", true);
        let deadline = Instant::now() + Duration::from_secs(5);
        while running.running().unwrap() {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(matches!(running.pump(&mut owner), Err(Error::Io)));
        assert!(!running.pipes[0].as_ref().unwrap().eof);
        running.first = Some(Error::Io);
        owner.record(Error::Io);
        let adopted = Cell::new(false);
        let result = running
            .finish_adopting(&mut owner, |bytes| {
                assert!(
                    std::str::from_utf8(bytes)
                        .unwrap()
                        .lines()
                        .any(|line| line == "a".repeat(64))
                );
                adopted.set(true);
                Ok(())
            })
            .await;
        assert!(matches!(result, Err(Error::Io)));
        assert!(adopted.get());
        assert_eq!(running.status(), Some(0));
        assert!(running.retired && running.reaped && running.closed);
        assert!(
            running
                .pipes
                .iter()
                .all(|p| p.as_ref().unwrap().file.is_none())
        );
        assert!(running.logs.iter().all(Option::is_none));
        owner.begin_settling();
        assert_eq!(owner.finish(), Err(Error::Io));
    }
    #[tokio::test(flavor = "current_thread")]
    async fn nonzero_child_output_supplies_no_creation_authority_and_all_peers_close() {
        let fixture = Fixture::new();
        let mut owner = Control::new(Instant::now()).unwrap();
        let mut running = fixture.spawn("nonzero", false);
        loop {
            let chunk = running.pump(&mut owner).unwrap();
            if chunk.stdout_eof && chunk.stderr_eof && !running.running().unwrap() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let adopted = Cell::new(false);
        let output = running
            .finish_adopting(&mut owner, |_| {
                adopted.set(true);
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(output.status, 7);
        assert!(!adopted.get());
        assert!(running.retired && running.reaped && running.closed);
        assert!(running.logs.iter().all(Option::is_none));
        owner.begin_settling();
        assert!(owner.finish().is_ok());
    }
    #[tokio::test(flavor = "current_thread")]
    async fn registered_startup_failure_keeps_completed_creation_callback_and_original_cause() {
        let fixture = Fixture::new();
        let mut owner = Control::new(Instant::now()).unwrap();
        let mut running = fixture.spawn("zero", true);
        let pid = running.pid();
        let deadline = Instant::now() + Duration::from_secs(5);
        while running.running().unwrap() {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            running.logs[0]
                .as_mut()
                .unwrap()
                .write_all(b"fault")
                .is_err()
        );
        running.first = Some(Error::Io);
        owner.record(Error::Io);
        let mut commands = Commands::new();
        commands.failed.push(running);
        let adopted = Cell::new(0);
        commands
            .settle_failed_start(&mut owner, 0, |bytes| {
                assert!(
                    std::str::from_utf8(bytes)
                        .unwrap()
                        .lines()
                        .any(|v| v == "a".repeat(64))
                );
                adopted.set(adopted.get() + 1);
                Ok(())
            })
            .await;
        assert_eq!(adopted.get(), 1);
        assert!(commands.failed.is_empty());
        assert!(!reserved(pid));
        assert_eq!(owner.first, Some(Error::Io));
        assert!(!owner.cleanup_failed);
        owner.begin_settling();
        assert_eq!(owner.finish(), Err(Error::Io));
    }
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "current_thread")]
    async fn retained_cloexec_script_remains_readable_by_its_shebang_interpreter() {
        let fixture = Fixture::new();
        let path = fixture.0.join("inert-script");
        fs::write(&path, b"#!/bin/sh\nprintf 'retained-script\\n'\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let mut owner = Control::new(Instant::now()).unwrap();
        let mut original = Original::fixture(&path, 1024, 0o755, &mut owner).unwrap();
        original
            .require_parent(rustix::process::getuid().as_raw(), 0o700, &mut owner)
            .unwrap();
        let mut command = Command::new(retained_executable(&original).unwrap());
        command.arg0(&path).env_clear();
        let mut running = fixture.spawn_command(command, false);
        let output = running
            .wait(&mut owner, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(output.status, 0);
        assert_eq!(output.stdout, b"retained-script\n");
        assert!(output.stderr.is_empty());
        assert!(running.retired && running.reaped && running.closed);
        assert!(running.logs.iter().all(Option::is_none));
        original.finish(&mut owner).unwrap();
        owner.begin_settling();
        owner.finish().unwrap();
    }
}
