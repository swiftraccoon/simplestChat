//! One fixed disposable Debian VM, private SSH identity and always-joined readers.

#[path = "prepared.rs"]
mod prepared;
#[path = "qmp.rs"]
mod qmp;
#[path = "session.rs"]
mod session;
pub(super) use session::Session;

use super::{
    Error, Result,
    control::Control,
    files::{self, Original},
    process::Runner,
};
use serde_json::{Value, json};
use session::Pipe;
use sha2::{Digest, Sha256, Sha512};
use simplestchat_ops::owned_process::{Group, exited_now, finish_before, reserved};
use std::{
    ffi::OsString,
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Write},
    net::TcpListener,
    os::{
        fd::OwnedFd,
        unix::{
            fs::{FileExt, FileTypeExt, MetadataExt, OpenOptionsExt},
            process::ExitStatusExt,
        },
    },
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::task::JoinHandle;

const GIB: u64 = 1024 * 1024 * 1024;
const IMAGE_MAX: u64 = 2 * GIB;
const STREAM_MAX: usize = 2 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(100);

fn watchdog_seconds(end: Instant, now: Instant) -> Result<u64> {
    let seconds = end.saturating_duration_since(now).as_secs().min(1800);
    if seconds == 0 {
        Err(Error::Deadline)
    } else {
        Ok(seconds)
    }
}
fn remaining_streams(observed: [usize; 2]) -> Result<[usize; 2]> {
    Ok([
        STREAM_MAX.checked_sub(observed[0]).ok_or(Error::Refused)?,
        STREAM_MAX.checked_sub(observed[1]).ok_or(Error::Refused)?,
    ])
}

fn remember(first: &mut Option<Error>, failure: &Arc<Mutex<Option<Error>>>, result: Result<()>) {
    if let Err(error) = result {
        first.get_or_insert(error);
        failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert(error);
    }
}
fn live(end: Instant) -> Result<()> {
    if Instant::now() >= end {
        Err(Error::Deadline)
    } else {
        Ok(())
    }
}
fn close(file: File) -> Result<()> {
    nix::unistd::close(file).map_err(|_| Error::Close)
}

struct TaskGuard {
    health: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<Error>>>,
    completed: bool,
}
impl Drop for TaskGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.failure
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get_or_insert(Error::Close);
        }
        self.health.store(false, Ordering::SeqCst);
    }
}

pub(super) fn preflight(work: &Path, control: &mut Control) -> Result<()> {
    control.check()?;
    let uname = rustix::system::uname();
    if uname.sysname() != c"Linux"
        || uname.machine() != c"x86_64"
        || rustix::process::geteuid().is_root()
    {
        return Err(Error::Refused);
    }
    let kvm = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(
            rustix::fs::OFlags::NOFOLLOW.bits() as i32 | rustix::fs::OFlags::CLOEXEC.bits() as i32,
        )
        .open("/dev/kvm")
        .map_err(|_| Error::Refused)?;
    let checked = kvm.metadata().map_err(|_| Error::Io).and_then(|m| {
        if m.file_type().is_char_device() {
            control.check()
        } else {
            Err(Error::Refused)
        }
    });
    if let Err(error) = checked {
        control.record(error);
    }
    let closed = files::close(kvm, control);
    checked?;
    closed?;
    let disk = rustix::fs::statvfs(work).map_err(|_| Error::Io)?;
    if u128::from(disk.f_bavail) * u128::from(disk.f_frsize) < u128::from(24 * GIB) {
        return Err(Error::Refused);
    }
    control.check()?;
    let mut file = File::open("/proc/meminfo").map_err(|_| Error::Io)?;
    let memory: Result<Vec<u8>> = (|| {
        let mut data = Vec::new();
        let mut block = [0; 4096];
        loop {
            control.check()?;
            let count = match file.read(&mut block) {
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                value => value.map_err(|_| Error::Io)?,
            };
            control.check()?;
            if count == 0 {
                break;
            }
            data.extend_from_slice(&block[..count]);
            if data.len() > 65536 {
                return Err(Error::Refused);
            }
        }
        Ok(data)
    })();
    if let Err(error) = memory {
        control.record(error);
    }
    let closed = files::close(file, control);
    let memory = memory?;
    closed?;
    let text = std::str::from_utf8(&memory).map_err(|_| Error::Refused)?;
    let available = text
        .lines()
        .filter_map(|line| line.strip_prefix("MemAvailable:"))
        .map(|value| value.split_whitespace().collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let [fields] = available.as_slice() else {
        return Err(Error::Refused);
    };
    if fields.len() != 2
        || fields[1] != "kB"
        || !fields[0].bytes().all(|b| b.is_ascii_digit())
        || fields[0].parse::<u64>().map_err(|_| Error::Refused)? < 5 * 1024 * 1024
    {
        return Err(Error::Refused);
    }
    control.check()
}

fn pin(data: &[u8]) -> Result<(String, String)> {
    let row = super::decode(data)?;
    let object = row.as_object().ok_or(Error::Refused)?;
    let expected = [
        "schemaVersion",
        "distribution",
        "architecture",
        "url",
        "sha512",
        "checksumsUrl",
    ];
    if object.len() != expected.len()
        || expected.iter().any(|name| !object.contains_key(*name))
        || row["schemaVersion"].as_u64() != Some(1)
        || row["distribution"] != "Debian 13"
        || row["architecture"] != "amd64"
    {
        return Err(Error::Refused);
    }
    let url = row["url"].as_str().ok_or(Error::Refused)?;
    let expression = regex::Regex::new(concat!(
        "^https://cloud\\.debian\\.org/images/cloud/trixie/([0-9]{8}-[0-9]+)/",
        "debian-13-genericcloud-amd64-([0-9]{8}-[0-9]+)\\.qcow2$"
    ))
    .map_err(|_| Error::Refused)?;
    let parts = expression.captures(url).ok_or(Error::Refused)?;
    let digest = row["sha512"].as_str().ok_or(Error::Refused)?;
    if parts.get(1).map(|v| v.as_str()) != parts.get(2).map(|v| v.as_str())
        || row["checksumsUrl"]
            != format!(
                "{}/SHA512SUMS",
                url.rsplit_once('/').ok_or(Error::Refused)?.0
            )
        || digest.len() != 128
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Refused);
    }
    Ok((url.into(), digest.into()))
}

fn verify_cloud(original: &Original, expected: &str, control: &mut Control) -> Result<()> {
    if original.bytes == 0 || original.bytes > IMAGE_MAX {
        return Err(Error::Refused);
    }
    original.metadata(control)?;
    let mut hash = Sha512::new();
    let mut offset = 0;
    let mut bytes = [0; 65536];
    loop {
        control.check()?;
        let count = match original.file()?.read_at(&mut bytes, offset) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            value => value.map_err(|_| Error::Io)?,
        };
        control.check()?;
        if count == 0 {
            break;
        }
        offset += count as u64;
        if offset > IMAGE_MAX {
            return Err(Error::Refused);
        }
        hash.update(&bytes[..count]);
    }
    if offset != original.bytes || files::hex(&hash.finalize()) != expected {
        return Err(Error::Changed);
    }
    original.metadata(control)
}

fn quote(value: &str) -> Result<String> {
    if value.contains('\0') || value.len() > 4096 {
        return Err(Error::Input);
    }
    Ok(format!("'{}'", value.replace('\'', "'\\''")))
}
fn ssh_arguments(work: &Path, port: u16, command: &[String]) -> Result<Vec<OsString>> {
    if port < 1024 || command.is_empty() {
        return Err(Error::Input);
    }
    let mut arguments = vec!["-F".into(), "/dev/null".into()];
    for option in [
        "BatchMode=yes".into(),
        "IdentitiesOnly=yes".into(),
        "IdentityAgent=none".into(),
        "ForwardAgent=no".into(),
        "PasswordAuthentication=no".into(),
        "KbdInteractiveAuthentication=no".into(),
        "StrictHostKeyChecking=yes".into(),
        "GlobalKnownHostsFile=/dev/null".into(),
        format!(
            "UserKnownHostsFile={}",
            work.join("known_hosts").to_str().ok_or(Error::Input)?
        ),
        "ProxyCommand=none".into(),
        "ProxyJump=none".into(),
        "ControlMaster=no".into(),
        "ControlPath=none".into(),
        "ControlPersist=no".into(),
        "ConnectTimeout=5".into(),
        "ConnectionAttempts=1".into(),
        "ServerAliveInterval=5".into(),
        "ServerAliveCountMax=2".into(),
    ] {
        arguments.extend(["-o".into(), OsString::from(option)]);
    }
    arguments.extend([
        "-i".into(),
        work.join("client").into_os_string(),
        "-p".into(),
        port.to_string().into(),
        "fixture@127.0.0.1".into(),
        command
            .iter()
            .map(|s| quote(s))
            .collect::<Result<Vec<_>>>()?
            .join(" ")
            .into(),
    ]);
    Ok(arguments)
}

pub(super) async fn retire(
    group: &mut Group,
    end: Instant,
    grace: Duration,
) -> (bool, Result<ExitStatus>) {
    let pid = group.pid();
    let mut valid = reserved(pid);
    let mut permission_unproven = false;
    let terminal = exited_now(pid);
    if terminal == Ok(false) && valid {
        match rustix::process::kill_process_group(pid, rustix::process::Signal::TERM) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => {}
            Err(rustix::io::Errno::PERM) => permission_unproven = true,
            Err(_) => valid = false,
        }
        let phase = (Instant::now() + grace).min(end);
        while Instant::now() < phase {
            match exited_now(pid) {
                Ok(true) => break,
                Ok(false) => {}
                Err(_) => {
                    valid = false;
                    break;
                }
            }
            tokio::time::sleep(POLL.min(phase.saturating_duration_since(Instant::now()))).await;
        }
    } else if terminal.is_err() {
        valid = false;
    }
    let finished = finish_before(pid, end).await;
    // The existing exact-group final proof alone can discharge a narrow TERM/PERM race.
    if finished {
        permission_unproven = false;
    }
    valid &= finished && !permission_unproven;
    let reaped = group.reap().await.map_err(|_| Error::Close);
    (valid, reaped)
}

#[cfg(all(test, target_os = "linux"))]
thread_local! {
    static DISK_OPEN_FLAGS: std::cell::Cell<Option<rustix::fs::OFlags>> = const {
        std::cell::Cell::new(None)
    };
}

struct MutableDisk {
    file: Option<File>,
    path: PathBuf,
    before: Metadata,
}
impl MutableDisk {
    fn open(path: &Path, control: &mut Control) -> Result<Self> {
        control.check()?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(
                rustix::fs::OFlags::NOFOLLOW.bits() as i32
                    | rustix::fs::OFlags::CLOEXEC.bits() as i32
                    | rustix::fs::OFlags::NONBLOCK.bits() as i32,
            )
            .open(path)
            .map_err(|_| Error::Io)?;
        #[cfg(all(test, target_os = "linux"))]
        DISK_OPEN_FLAGS.set(rustix::fs::fcntl_getfl(&file).ok());
        let before = match file.metadata() {
            Ok(value) => value,
            Err(_) => {
                control.record(Error::Io);
                let _ = files::close(file, control);
                return Err(Error::Io);
            }
        };
        let mut value = Self {
            file: Some(file),
            path: path.into(),
            before,
        };
        if let Err(error) = value.check(control) {
            control.record(error);
            let _ = value.finish(control);
            return Err(error);
        }
        Ok(value)
    }
    fn check(&self, control: &mut Control) -> Result<()> {
        control.check()?;
        let held = self
            .file
            .as_ref()
            .ok_or(Error::Close)?
            .metadata()
            .map_err(|_| Error::Io)?;
        let visible = fs::symlink_metadata(&self.path).map_err(|_| Error::Changed)?;
        let same = |m: &Metadata| {
            m.is_file()
                && m.uid() == rustix::process::getuid().as_raw()
                && (m.dev(), m.ino(), m.mode(), m.uid(), m.gid(), m.nlink())
                    == (
                        self.before.dev(),
                        self.before.ino(),
                        self.before.mode(),
                        self.before.uid(),
                        self.before.gid(),
                        1,
                    )
                && m.len() <= 17 * GIB
        };
        if !same(&held) || !same(&visible) {
            return Err(Error::Changed);
        }
        control.check()
    }
    fn finish(&mut self, control: &mut Control) -> Result<()> {
        self.file
            .take()
            .map(|file| files::close(file, control))
            .unwrap_or(Ok(()))
    }
}

#[derive(Clone, Copy)]
enum StderrClass {
    HostForwardSetup,
    BackingFileMissing,
    Qcow2Format,
    ImageWriteLock,
    KvmAccess,
    Unclassified,
}
impl StderrClass {
    fn name(self) -> &'static str {
        match self {
            Self::HostForwardSetup => "host-forward-setup",
            Self::BackingFileMissing => "backing-file-missing",
            Self::Qcow2Format => "qcow2-format",
            Self::ImageWriteLock => "image-write-lock",
            Self::KvmAccess => "kvm-access",
            Self::Unclassified => "unclassified",
        }
    }
}

#[derive(Clone)]
struct StartupProfile {
    records: [(StderrClass, String); 5],
}
impl StartupProfile {
    fn new(arguments: &[OsString], work: &Path) -> Option<Self> {
        let option = |name: &str| {
            arguments.windows(2).find(|pair| pair[0] == name)?[1]
                .to_str()
                .map(str::to_owned)
        };
        let at = arguments
            .iter()
            .position(|value| value == "-name")?
            .checked_sub(1)?;
        let program = Path::new(&arguments[at]).file_name()?.to_str()?;
        let qmp = option("-qmp")?;
        let root = option("-blockdev")?;
        let network = option("-netdev")?;
        let forward = network.split_once("hostfwd=")?.1;
        let socket = qmp.strip_suffix(",wait=on")?;
        let wait = format!(
            "{program}: -qmp {qmp}: info: QEMU waiting for connection on: \
             disconnected:{socket}\n"
        );
        let block = format!("{program}: -blockdev {root}: ");
        let base = work.join("base.qcow2");
        let disk = work.join("guest.qcow2");
        let base = base.to_str()?;
        let disk = disk.to_str()?;
        Some(Self {
            records: [
                (
                    StderrClass::HostForwardSetup,
                    format!(
                        "{wait}{program}: -netdev {network}: Could not set up host forwarding rule \
                     '{forward}'\n"
                    ),
                ),
                (
                    StderrClass::BackingFileMissing,
                    format!(
                        "{wait}{block}Could not open backing file: Could not open '{base}': \
                     No such file or directory\n"
                    ),
                ),
                (
                    StderrClass::Qcow2Format,
                    format!("{wait}{block}Image is not in qcow2 format\n"),
                ),
                (
                    StderrClass::ImageWriteLock,
                    format!(
                        "{wait}{block}Failed to get \"write\" lock\n\
                     Is another process using the image [{disk}]?\n"
                    ),
                ),
                (
                    StderrClass::KvmAccess,
                    format!(
                        "{wait}Could not access KVM kernel module: Permission denied\n\
                         {program}: failed to initialize kvm: Permission denied\n"
                    ),
                ),
            ],
        })
    }
    fn classify(&self, bytes: &[u8]) -> StderrClass {
        self.records
            .iter()
            .find(|(_, record)| record.as_bytes() == bytes)
            .map(|(kind, _)| *kind)
            .unwrap_or(StderrClass::Unclassified)
    }
}

struct StderrObservation {
    class: StderrClass,
    sha256: String,
}

#[derive(Clone, Copy)]
enum ReaderFault {
    UnexpectedEof,
    Uncertain(Error),
}
#[derive(Default)]
struct ReaderFacts {
    first: Option<Error>,
    eof: [bool; 2],
    faulted: [bool; 2],
    closed: [bool; 4],
    bytes: [usize; 2],
    joined: bool,
    completed: bool,
    uncertain: bool,
    stderr: Option<StderrObservation>,
}
impl ReaderFacts {
    fn fail(
        &mut self,
        fault: ReaderFault,
        failure: &Arc<Mutex<Option<Error>>>,
        health: &AtomicBool,
    ) {
        let error = match fault {
            ReaderFault::UnexpectedEof => Error::Refused,
            ReaderFault::Uncertain(error) => {
                self.uncertain = true;
                self.stderr = None;
                error
            }
        };
        remember(&mut self.first, failure, Err(error));
        health.store(false, Ordering::SeqCst);
    }
    fn evidence_uncertain(&mut self) {
        self.uncertain = true;
        self.stderr = None;
    }
}
struct ReaderOwned {
    pipes: [Option<Pipe>; 2],
    logs: [Option<File>; 2],
}
impl ReaderOwned {
    fn pump(
        &mut self,
        facts: &mut ReaderFacts,
        failure: &Arc<Mutex<Option<Error>>>,
        health: &AtomicBool,
        closing: &AtomicBool,
        limits: [usize; 2],
        end: Instant,
    ) -> Result<bool> {
        if let Err(error) = live(end) {
            facts.fail(ReaderFault::Uncertain(error), failure, health);
            return Err(error);
        }
        let mut block = [0; 65536];
        for (index, limit) in limits.into_iter().enumerate() {
            if facts.eof[index] || facts.faulted[index] {
                continue;
            }
            let observed: Result<()> = (|| {
                live(end)?;
                let pipe = self.pipes[index].as_mut().ok_or(Error::Close)?;
                let count = pipe.read(&mut block)?;
                facts.eof[index] = pipe.eof;
                facts.bytes[index] = pipe.bytes;
                live(end)?;
                let Some(count) = count else {
                    return Ok(());
                };
                if pipe.bytes > limit {
                    return Err(Error::Refused);
                }
                if count == 0 {
                    if !closing.load(Ordering::SeqCst) {
                        facts.fail(ReaderFault::UnexpectedEof, failure, health);
                    }
                    return Ok(());
                }
                let output = self.logs[index].as_mut().ok_or(Error::Close)?;
                let mut written = 0;
                while written < count {
                    live(end)?;
                    let n = match output.write(&block[written..count]) {
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        value => value.map_err(|_| Error::Io)?,
                    };
                    live(end)?;
                    if n == 0 {
                        return Err(Error::Io);
                    }
                    written += n;
                }
                Ok(())
            })();
            if let Err(error) = observed {
                facts.faulted[index] = true;
                facts.fail(ReaderFault::Uncertain(error), failure, health);
                if matches!(error, Error::Deadline) {
                    return Err(error);
                }
            }
        }
        Ok((0..2).all(|index| facts.eof[index] || facts.faulted[index]))
    }
    fn stderr_observation(
        &self,
        facts: &ReaderFacts,
        profile: Option<&StartupProfile>,
        end: Instant,
    ) -> Result<StderrObservation> {
        live(end)?;
        if facts.uncertain || !facts.eof.iter().all(|value| *value) {
            return Err(Error::Refused);
        }
        let file = self.logs[1].as_ref().ok_or(Error::Close)?;
        let before = file.metadata().map_err(|_| Error::Io)?;
        if !before.is_file()
            || before.nlink() != 1
            || before.uid() != rustix::process::getuid().as_raw()
            || before.mode() & 0o7777 != 0o600
            || before.len() != facts.bytes[1] as u64
        {
            return Err(Error::Changed);
        }
        let mut hash = Sha256::new();
        let mut prefix = Vec::new();
        let mut offset = 0;
        let mut block = [0; 65536];
        while offset < before.len() {
            live(end)?;
            let maximum = (before.len() - offset).min(block.len() as u64) as usize;
            let count = match file.read_at(&mut block[..maximum], offset) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                value => value.map_err(|_| Error::Io)?,
            };
            live(end)?;
            if count == 0 {
                return Err(Error::Io);
            }
            offset += count as u64;
            hash.update(&block[..count]);
            if before.len() <= 8192 {
                prefix.extend_from_slice(&block[..count]);
            }
        }
        if !files::same(&before, &file.metadata().map_err(|_| Error::Io)?) {
            return Err(Error::Changed);
        }
        live(end)?;
        Ok(StderrObservation {
            class: profile
                .map(|profile| profile.classify(&prefix))
                .unwrap_or(StderrClass::Unclassified),
            sha256: files::hex(&hash.finalize()),
        })
    }
    fn finish(&mut self, facts: &mut ReaderFacts, failure: &Arc<Mutex<Option<Error>>>) {
        for (i, pipe) in self.pipes.iter_mut().enumerate() {
            if let Some(pipe) = pipe {
                let closed = pipe.finish();
                if closed.is_err() {
                    facts.evidence_uncertain();
                }
                remember(&mut facts.first, failure, closed);
                facts.closed[i] = pipe.closed;
            }
        }
        for (i, file) in self.logs.iter_mut().enumerate() {
            if let Some(mut file) = file.take() {
                let flushed = file.flush().map_err(|_| Error::Io);
                if flushed.is_err() {
                    facts.evidence_uncertain();
                }
                remember(&mut facts.first, failure, flushed);
                let result = close(file);
                if result.is_err() {
                    facts.evidence_uncertain();
                }
                facts.closed[i + 2] = result.is_ok();
                remember(&mut facts.first, failure, result);
            }
        }
    }
}

pub(super) struct Vm {
    work: PathBuf,
    port: u16,
    end: Instant,
    run_id: String,
    restricted: bool,
    preparation: Option<Value>,
    handoff: Option<prepared::Prepared>,
    stream_limits: [usize; 2],
    qmp_limits: qmp::Limits,
    originals: Vec<Original>,
    overlay: Option<MutableDisk>,
    group: Option<Group>,
    qmp: Option<qmp::Observer>,
    reader: Option<JoinHandle<()>>,
    monitor: Option<JoinHandle<()>>,
    owned: Arc<Mutex<ReaderOwned>>,
    facts: Arc<Mutex<ReaderFacts>>,
    health: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<Error>>>,
    closing: Arc<AtomicBool>,
    stop_reader: Arc<AtomicBool>,
    authenticated: Arc<AtomicBool>,
    startup_profile: Option<StartupProfile>,
    cloud_init: bool,
    first: Option<Error>,
    status: Option<i32>,
    retired: bool,
    reaped: bool,
    closed: bool,
}

impl Vm {
    fn epoch(work: PathBuf, port: u16, end: Instant, run_id: String, restricted: bool) -> Self {
        Self {
            work,
            port,
            end,
            run_id,
            restricted,
            preparation: None,
            handoff: None,
            stream_limits: [STREAM_MAX; 2],
            qmp_limits: qmp::Limits::default(),
            originals: Vec::new(),
            overlay: None,
            group: None,
            qmp: None,
            reader: None,
            monitor: None,
            owned: Arc::new(Mutex::new(ReaderOwned {
                pipes: [None, None],
                logs: [None, None],
            })),
            facts: Arc::new(Mutex::new(ReaderFacts::default())),
            health: Arc::new(AtomicBool::new(true)),
            failure: Arc::new(Mutex::new(None)),
            closing: Arc::new(AtomicBool::new(false)),
            stop_reader: Arc::new(AtomicBool::new(false)),
            authenticated: Arc::new(AtomicBool::new(false)),
            startup_profile: None,
            cloud_init: false,
            first: None,
            status: None,
            retired: false,
            reaped: false,
            closed: false,
        }
    }
    pub async fn create(
        runner: &mut Runner,
        control: &mut Control,
        work: &Path,
        run_id: &str,
    ) -> Result<Self> {
        if control.settling {
            return Err(Error::Refused);
        }
        preflight(work, control)?;
        if run_id.len() != 32
            || !run_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || fs::canonicalize(work).map_err(|_| Error::Io)? != work
            || work.join("qmp.sock").as_os_str().as_encoded_bytes().len() > 100
        {
            return Err(Error::Input);
        }
        let directory = fs::symlink_metadata(work).map_err(|_| Error::Io)?;
        if !directory.is_dir()
            || directory.uid() != rustix::process::getuid().as_raw()
            || directory.mode() & 0o777 != 0o700
        {
            return Err(Error::Refused);
        }
        // No reader task can be admitted without the caller's already entered runtime.
        tokio::runtime::Handle::try_current().map_err(|_| Error::Refused)?;
        control.check()?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|_| Error::Io)?;
        let selected = listener
            .local_addr()
            .map(|address| address.port())
            .map_err(|_| Error::Io);
        if let Err(error) = selected {
            control.record(error);
        }
        let closed = nix::unistd::close(listener).map_err(|_| Error::Close);
        if let Err(error) = closed {
            control.cleanup_failed = true;
            control.record(error);
        }
        let port = selected?;
        closed?;
        control.check()?;
        let mut vm = Self::epoch(work.into(), port, control.end(), run_id.into(), false);
        let result = vm.prepare(runner, control, run_id).await;
        if let Err(error) = result {
            let observed = *vm
                .failure
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let error = observed.unwrap_or(error);
            remember(&mut vm.first, &vm.failure, Err(error));
            control.record(error);
            if vm.finish(control).await.is_err() {
                control.cleanup_failed = true;
            }
            return Err(error);
        }
        Ok(vm)
    }
    fn hold(&mut self, name: &str, maximum: u64, control: &mut Control) -> Result<usize> {
        let original = Original::open(&self.work.join(name), maximum, control)?;
        self.originals.push(original);
        Ok(self.originals.len() - 1)
    }
    async fn prepare(
        &mut self,
        runner: &mut Runner,
        control: &mut Control,
        run_id: &str,
    ) -> Result<()> {
        let (url, expected) = pin(&runner.source("security/vm/debian-cloud.json", control)?)?;
        let image = self.work.join("base.qcow2");
        runner
            .run(
                control,
                "download-cloud",
                "curl",
                vec![
                    "--disable".into(),
                    "--fail".into(),
                    "--silent".into(),
                    "--show-error".into(),
                    "--location".into(),
                    "--proto".into(),
                    "=https".into(),
                    "--proto-redir".into(),
                    "=https".into(),
                    "--connect-timeout".into(),
                    "15".into(),
                    "--max-time".into(),
                    "300".into(),
                    "--max-filesize".into(),
                    IMAGE_MAX.to_string().into(),
                    "--output".into(),
                    image.clone().into_os_string(),
                    url.into(),
                ],
                Duration::from_secs(310),
                &[0],
            )
            .await?;
        let base = self.hold("base.qcow2", IMAGE_MAX, control)?;
        verify_cloud(&self.originals[base], &expected, control)?;
        let info = runner
            .run(
                control,
                "cloud-image-info",
                "qemu-img",
                vec![
                    "info".into(),
                    "--output=json".into(),
                    image.clone().into_os_string(),
                ],
                Duration::from_secs(120),
                &[0],
            )
            .await?;
        let value = super::decode(&info.stdout)?;
        if value["format"] != "qcow2"
            || value["virtual-size"]
                .as_u64()
                .is_none_or(|n| n == 0 || n > 16 * GIB)
            || value
                .get("backing-filename")
                .is_some_and(|v| !v.is_null() && v != "")
            || value
                .get("snapshots")
                .is_some_and(|v| !v.is_null() && v != &json!([]))
        {
            return Err(Error::Refused);
        }
        self.originals[base].metadata(control)?;
        runner
            .run(
                control,
                "cloud-overlay",
                "qemu-img",
                vec![
                    "create".into(),
                    "-f".into(),
                    "qcow2".into(),
                    "-F".into(),
                    "qcow2".into(),
                    "-b".into(),
                    image.into_os_string(),
                    self.work.join("guest.qcow2").into_os_string(),
                    "16G".into(),
                ],
                Duration::from_secs(120),
                &[0],
            )
            .await?;
        self.overlay = Some(MutableDisk::open(&self.work.join("guest.qcow2"), control)?);
        for name in ["client", "host"] {
            runner
                .run(
                    control,
                    &format!("key-{name}"),
                    "ssh-keygen",
                    vec![
                        "-q".into(),
                        "-t".into(),
                        "ed25519".into(),
                        "-N".into(),
                        "".into(),
                        "-C".into(),
                        "vm-fixture".into(),
                        "-f".into(),
                        self.work.join(name).into_os_string(),
                    ],
                    Duration::from_secs(120),
                    &[0],
                )
                .await?;
            self.hold(name, 8192, control)?;
            self.hold(&format!("{name}.pub"), 4096, control)?;
        }
        let read = |name: &str, originals: &[Original], control: &mut Control| -> Result<Vec<u8>> {
            originals
                .iter()
                .find(|o| o.path.file_name().is_some_and(|n| n == name))
                .ok_or(Error::Refused)?
                .data(8192, control)
        };
        let key = regex::Regex::new("^ssh-ed25519 [A-Za-z0-9+/=]+ vm-fixture$")
            .map_err(|_| Error::Refused)?;
        let client = String::from_utf8(read("client.pub", &self.originals, control)?)
            .map_err(|_| Error::Refused)?;
        let host = String::from_utf8(read("host.pub", &self.originals, control)?)
            .map_err(|_| Error::Refused)?;
        let client = client.trim();
        let host = host.trim();
        if !key.is_match(client) || !key.is_match(host) {
            return Err(Error::Refused);
        }
        let private = String::from_utf8(read("host", &self.originals, control)?)
            .map_err(|_| Error::Refused)?;
        let config = json!({"hostname":"simplestchat-fixture",
            "ssh_pwauth":false,"disable_root":true,
            "ssh_keys":{"ed25519_private":private,"ed25519_public":host},
            "users":[{"name":"fixture","lock_passwd":true,"shell":"/bin/bash",
                "sudo":["ALL=(ALL) NOPASSWD:ALL"],"ssh_authorized_keys":[client]}],
            "write_files":[{"path":"/etc/simplestchat-vm-fixture",
                "owner":"root:root",
                "permissions":"0600","content":format!("{run_id}\n")}]});
        let mut user = b"#cloud-config\n".to_vec();
        user.extend(serde_json::to_vec(&config).map_err(|_| Error::Input)?);
        user.push(b'\n');
        files::write(&self.work.join("user-data"), &user, control)?;
        files::write(
            &self.work.join("meta-data"),
            &serde_json::to_vec(&json!({
            "instance-id":run_id,"local-hostname":"simplestchat-fixture"}))
            .map_err(|_| Error::Input)?,
            control,
        )?;
        files::write(
            &self.work.join("known_hosts"),
            format!("[127.0.0.1]:{} {host}\n", self.port).as_bytes(),
            control,
        )?;
        for name in ["user-data", "meta-data", "known_hosts"] {
            self.hold(name, 16384, control)?;
        }
        runner
            .run(
                control,
                "cloud-seed",
                "cloud-localds",
                vec![
                    self.work.join("seed.img").into_os_string(),
                    self.work.join("user-data").into_os_string(),
                    self.work.join("meta-data").into_os_string(),
                ],
                Duration::from_secs(120),
                &[0],
            )
            .await?;
        self.hold("seed.img", 16 * 1024 * 1024, control)?;
        self.launch(runner, control, run_id)?;
        runner.set_guest_health(self.health(), self.failure());
        self.boot(runner, control).await
    }
    fn launch(&mut self, runner: &mut Runner, control: &mut Control, run_id: &str) -> Result<()> {
        self.metadata(control)?;
        let prlimit = runner.tool("prlimit", control)?;
        let qemu = runner.tool("qemu-system-x86_64", control)?;
        let label = if self.restricted {
            "qemu-restricted"
        } else {
            "qemu"
        };
        let stdout = runner.log(label, "stdout", control)?;
        let stderr = match runner.log(label, "stderr", control) {
            Ok(value) => value,
            Err(error) => {
                control.record(error);
                let _ = files::close(stdout, control);
                return Err(error);
            }
        };
        {
            let mut owned = self.owned.lock().map_err(|_| Error::Io)?;
            owned.logs = [Some(stdout), Some(stderr)];
        }
        let mut command = runner.command("timeout", &[], control)?;
        if let Some(handoff) = &self.handoff {
            handoff.verify_ready(self, control)?;
        }
        if self.end != control.end() {
            return Err(Error::Changed);
        }
        let seconds = watchdog_seconds(self.end, Instant::now())?;
        let arguments = qemu_arguments(
            &self.work,
            self.port,
            run_id,
            prlimit,
            qemu,
            self.restricted,
            seconds,
        )?;
        self.startup_profile = StartupProfile::new(&arguments, &self.work);
        command
            .args(arguments)
            .env("MALLOC_ARENA_MAX", "2")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        control.check()?;
        self.group = Some(Group::new(command.spawn().map_err(|_| Error::Io)?));
        // The frozen disk witness ends only after the actual writer is owned.
        self.handoff = None;
        let child = self.group.as_mut().ok_or(Error::Close)?.child();
        let files = [
            child.stdout.take().map(|p| File::from(OwnedFd::from(p))),
            child.stderr.take().map(|p| File::from(OwnedFd::from(p))),
        ];
        let mut first = None;
        {
            let mut owned = self.owned.lock().map_err(|_| Error::Io)?;
            for (index, file) in files.into_iter().enumerate() {
                match file.map(|file| Pipe::new(file, control)).transpose() {
                    Ok(Some(pipe)) => owned.pipes[index] = Some(pipe),
                    Ok(None) => {
                        first.get_or_insert(Error::Close);
                        control.record(Error::Close);
                    }
                    Err(error) => {
                        first.get_or_insert(error);
                        control.record(error);
                    }
                }
            }
        }
        if let Some(error) = first {
            return Err(error);
        }
        control.check()?;
        self.start_readers();
        let qmp_path = self.work.join(if self.restricted {
            "qmp-restricted.sock"
        } else {
            "qmp.sock"
        });
        self.qmp = Some(if self.restricted {
            qmp::Observer::start_limited(
                &qmp_path,
                self.end,
                self.health.clone(),
                self.closing.clone(),
                self.failure.clone(),
                self.qmp_limits,
            )?
        } else {
            qmp::Observer::start(
                &qmp_path,
                self.end,
                self.health.clone(),
                self.closing.clone(),
                self.failure.clone(),
            )?
        });
        control.check()
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn health(&self) -> Arc<AtomicBool> {
        self.health.clone()
    }
    pub fn failure(&self) -> Arc<Mutex<Option<Error>>> {
        self.failure.clone()
    }
    pub fn preparation_diagnostics(&self) -> Option<Value> {
        self.preparation.clone()
    }

    /// Settle the prepared VM before admitting a fresh, restricted provider.
    pub async fn restart_restricted(
        &mut self,
        runner: &mut Runner,
        control: &mut Control,
        run_id: &str,
    ) -> Result<Value> {
        let captured = async {
            control.check()?;
            if !control.setup
                || control.settling
                || self.restricted
                || self.closed
                || self.preparation.is_some()
                || self.handoff.is_some()
                || self.end != control.end()
                || self.run_id != run_id
            {
                return Err(Error::Refused);
            }
            self.check(control)?;
            runner.check(control)?;
            // Guest writes must reach the prepared root filesystem before the
            // QEMU writer is stopped and its overlay becomes immutable input.
            let arguments = self.ssh(
                runner,
                control,
                &[
                    "/usr/bin/sudo".into(),
                    "-n".into(),
                    "/usr/bin/sync".into(),
                    "-f".into(),
                    "/".into(),
                ],
            )?;
            runner
                .run(
                    control,
                    "prepared-root-sync",
                    "ssh",
                    arguments,
                    Duration::from_secs(30),
                    &[0],
                )
                .await?;
            self.check(control)?;
            self.handoff = Some(prepared::Prepared::capture(self, control)?);
            Ok(())
        }
        .await;
        if let Err(error) = captured {
            control.record(error);
            let _ = self.finish(control).await;
            self.preparation = Some(self.diagnostics());
            return Err(error);
        }
        let settled = self.finish_epoch(control, true).await;
        let first = self.diagnostics();
        self.preparation = Some(first.clone());
        if let Err(error) = settled {
            control.record(error);
            let _ = self.finish(control).await;
            return Err(error);
        }
        let remaining = (|| {
            control.check()?;
            let facts = self.facts.lock().map_err(|_| Error::Close)?;
            let streams = remaining_streams(facts.bytes)?;
            let qmp = self.qmp.as_ref().ok_or(Error::Close)?.remaining()?;
            Ok((streams, qmp))
        })();
        let (stream_limits, qmp_limits) = match remaining {
            Ok(value) => value,
            Err(error) => {
                control.record(error);
                remember(&mut self.first, &self.failure, Err(error));
                let _ = self.finish(control).await;
                return Err(error);
            }
        };
        let old_health = self.health();
        let old_failure = self.failure();
        let handoff = self.handoff.take();
        let mut next = Self::epoch(
            self.work.clone(),
            self.port,
            self.end,
            self.run_id.clone(),
            true,
        );
        next.preparation = Some(first.clone());
        next.handoff = handoff;
        next.stream_limits = stream_limits;
        next.qmp_limits = qmp_limits;
        // The old epoch has no remaining live process, task, pipe or original.
        *self = next;
        let restarted = async {
            runner.replace_guest_health(
                &old_health,
                &old_failure,
                self.health(),
                self.failure(),
                control,
            )?;
            let mut handoff = self.handoff.take().ok_or(Error::Changed)?;
            let admitted = handoff.admit(self, control);
            self.handoff = Some(handoff);
            admitted?;
            let finished = self.handoff.as_mut().ok_or(Error::Changed)?.finish(control);
            finished?;
            self.launch(runner, control, run_id)?;
            self.boot(runner, control).await?;
            self.check(control)
        }
        .await;
        if let Err(error) = restarted {
            control.record(error);
            remember(&mut self.first, &self.failure, Err(error));
            if self.finish(control).await.is_err() {
                control.cleanup_failed = true;
            }
            return Err(error);
        }
        Ok(first)
    }
    fn metadata(&self, control: &mut Control) -> Result<()> {
        if let Some(error) = *self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            control.record(error);
        }
        control.check()?;
        for original in &self.originals {
            original.metadata(control)?;
        }
        if let Some(disk) = &self.overlay {
            disk.check(control)?;
        }
        control.check()
    }
    pub fn check(&mut self, control: &mut Control) -> Result<()> {
        self.metadata(control)?;
        if self.closed || !self.health.load(Ordering::SeqCst) {
            return Err(Error::Refused);
        }
        if let Some(group) = &self.group {
            if exited_now(group.pid()).map_err(|_| Error::Close)? {
                return Err(Error::Refused);
            }
        } else {
            return Err(Error::Close);
        }
        if let Some(observer) = &self.qmp {
            observer.check()?;
        } else {
            return Err(Error::Refused);
        }
        control.check()
    }
    pub fn ssh(
        &self,
        runner: &Runner,
        control: &mut Control,
        command: &[String],
    ) -> Result<Vec<OsString>> {
        if control.settling {
            return Err(Error::Refused);
        }
        self.metadata(control)?;
        let _ = runner.tool("ssh", control)?;
        if !self.health.load(Ordering::SeqCst) || self.closed {
            return Err(Error::Refused);
        }
        ssh_arguments(&self.work, self.port, command)
    }
    async fn boot(&mut self, runner: &mut Runner, control: &mut Control) -> Result<()> {
        for attempt in 0..24 {
            self.check(control)?;
            let arguments = self.ssh(runner, control, &["/usr/bin/true".into()])?;
            let result = runner
                .run(
                    control,
                    "ssh-ready",
                    "ssh",
                    arguments,
                    Duration::from_secs(8),
                    &[0, 255],
                )
                .await?;
            self.check(control)?;
            if result.status == 0 {
                self.authenticated.store(true, Ordering::SeqCst);
                break;
            }
            if attempt == 23 {
                return Err(Error::Refused);
            }
            tokio::time::sleep(
                Duration::from_secs(2).min(control.remaining(Duration::from_secs(2))?),
            )
            .await;
            control.check()?;
        }
        let arguments = self.ssh(
            runner,
            control,
            &[
                "sudo".into(),
                "-n".into(),
                "cloud-init".into(),
                "status".into(),
                "--wait".into(),
                "--format=json".into(),
            ],
        )?;
        let result = runner
            .run(
                control,
                "cloud-init",
                "ssh",
                arguments,
                Duration::from_secs(180),
                &[0, 1, 2],
            )
            .await?;
        self.cloud_init = cloud_passed(&result.stdout, result.status)?;
        if !self.cloud_init {
            return Err(Error::Refused);
        }
        self.check(control)
    }
}

fn qemu_arguments(
    work: &Path,
    port: u16,
    run_id: &str,
    prlimit: OsString,
    qemu: OsString,
    restricted: bool,
    seconds: u64,
) -> Result<Vec<OsString>> {
    if port < 1024
        || !(1..=1800).contains(&seconds)
        || !files::safe_path(work)
        || run_id.len() != 32
        || !run_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || work
            .join(if restricted {
                "qmp-restricted.sock"
            } else {
                "qmp.sock"
            })
            .as_os_str()
            .as_encoded_bytes()
            .len()
            > 100
    {
        return Err(Error::Input);
    }
    let path = |name: &str| {
        work.join(name)
            .to_str()
            .map(str::to_owned)
            .ok_or(Error::Input)
    };
    Ok(vec![
        "--signal=TERM".into(),
        "--kill-after=5s".into(),
        format!("{seconds}s").into(),
        prlimit,
        format!("--as={}", 5 * GIB).into(),
        format!("--fsize={}", 17 * GIB).into(),
        "--cpu=3600".into(),
        "--nofile=1024".into(),
        "--nproc=512".into(),
        "--core=0".into(),
        "--".into(),
        qemu,
        "-name".into(),
        format!("simplestchat-ci-{run_id}").into(),
        "-no-user-config".into(),
        "-nodefaults".into(),
        "-machine".into(),
        "q35,accel=kvm".into(),
        "-cpu".into(),
        "host".into(),
        "-smp".into(),
        "2".into(),
        "-m".into(),
        "3072".into(),
        "-object".into(),
        "main-loop,id=ci-main-loop,thread-pool-max=8".into(),
        "-display".into(),
        "none".into(),
        "-device".into(),
        "VGA".into(),
        "-monitor".into(),
        "none".into(),
        "-qmp".into(),
        format!(
            "unix:{},server=on,wait=on",
            path(if restricted {
                "qmp-restricted.sock"
            } else {
                "qmp.sock"
            })?
            .replace(',', ",,")
        )
        .into(),
        "-serial".into(),
        "stdio".into(),
        "-sandbox".into(),
        "on,obsolete=deny,elevateprivileges=deny,spawn=deny,resourcecontrol=deny".into(),
        "-blockdev".into(),
        json!({"driver":"qcow2","node-name":"rootdisk",
            "file":{"driver":"file","filename":path("guest.qcow2")?}})
        .to_string()
        .into(),
        "-device".into(),
        "virtio-blk-pci,drive=rootdisk".into(),
        "-blockdev".into(),
        json!({"driver":"raw","node-name":"seed","read-only":true,
            "file":{"driver":"file","filename":path("seed.img")?}})
        .to_string()
        .into(),
        "-device".into(),
        "virtio-blk-pci,drive=seed".into(),
        "-netdev".into(),
        if restricted {
            format!("user,id=net0,restrict=on,ipv4=on,ipv6=off,hostfwd=tcp:127.0.0.1:{port}-:22")
        } else {
            format!("user,id=net0,ipv4=on,ipv6=off,hostfwd=tcp:127.0.0.1:{port}-:22")
        }
        .into(),
        "-device".into(),
        "virtio-net-pci,netdev=net0".into(),
    ])
}

impl Vm {
    fn start_readers(&mut self) {
        let owned = self.owned.clone();
        let facts = self.facts.clone();
        let health = self.health.clone();
        let closing = self.closing.clone();
        let stop = self.stop_reader.clone();
        let end = self.end;
        let limits = self.stream_limits;
        let failure = self.failure.clone();
        let guard = TaskGuard {
            health: health.clone(),
            failure: failure.clone(),
            completed: false,
        };
        let authenticated = self.authenticated.clone();
        let profile = self.startup_profile.clone();
        self.reader = Some(tokio::spawn(async move {
            let mut guard = guard;
            let result: Result<()> = async {
                loop {
                    live(end)?;
                    if stop.load(Ordering::SeqCst) {
                        return Err(Error::Close);
                    }
                    let complete = {
                        let mut owned = owned.lock().map_err(|_| Error::Io)?;
                        let mut facts = facts.lock().map_err(|_| Error::Io)?;
                        owned.pump(&mut facts, &failure, &health, &closing, limits, end)?
                    };
                    if complete {
                        return live(end);
                    }
                    tokio::time::sleep(POLL.min(end.saturating_duration_since(Instant::now())))
                        .await;
                }
            }
            .await;
            let mut data = facts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Err(error) = result {
                data.fail(ReaderFault::Uncertain(error), &failure, &health);
            }
            data.completed = result.is_ok() && data.first.is_none();
            let mut owned = owned
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !authenticated.load(Ordering::SeqCst)
                && !data.uncertain
                && data.eof.iter().all(|value| *value)
            {
                match owned.stderr_observation(&data, profile.as_ref(), end) {
                    Ok(observed) => data.stderr = Some(observed),
                    Err(error) => data.fail(ReaderFault::Uncertain(error), &failure, &health),
                }
            }
            owned.finish(&mut data, &failure);
            if let Err(error) = live(end) {
                data.fail(ReaderFault::Uncertain(error), &failure, &health);
            }
            if let Some(error) = data.first {
                failure
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get_or_insert(error);
                health.store(false, Ordering::SeqCst);
            }
            guard.completed = true;
        }));
        if let Some(group) = &self.group {
            let pid = group.pid();
            let health = self.health.clone();
            let closing = self.closing.clone();
            let observed_failure = self.failure.clone();
            let guard = TaskGuard {
                health: health.clone(),
                failure: observed_failure.clone(),
                completed: false,
            };
            self.monitor = Some(tokio::spawn(async move {
                let mut guard = guard;
                while !closing.load(Ordering::SeqCst) {
                    let failure = if Instant::now() >= end {
                        Some(Error::Deadline)
                    } else {
                        match exited_now(pid) {
                            Ok(false) => None,
                            Ok(true) => Some(Error::Refused),
                            Err(_) => Some(Error::Close),
                        }
                    };
                    if let Some(error) = failure {
                        observed_failure
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .get_or_insert(error);
                        health.store(false, Ordering::SeqCst);
                        guard.completed = true;
                        return;
                    }
                    tokio::time::sleep(POLL.min(end.saturating_duration_since(Instant::now())))
                        .await;
                }
                guard.completed = true;
            }));
        }
    }
    pub async fn finish(&mut self, control: &mut Control) -> Result<Value> {
        self.finish_epoch(control, false).await
    }
    async fn finish_epoch(&mut self, control: &mut Control, keep_handoff: bool) -> Result<Value> {
        let settlement_end = if keep_handoff {
            control.body_end().min(self.end)
        } else {
            self.end
        };
        if !self.closed {
            let observed = *self
                .failure
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.first = self.first.or(observed);
            self.closed = true;
            self.closing.store(true, Ordering::SeqCst);
            self.health.store(false, Ordering::SeqCst);
            if let Some(mut monitor) = self.monitor.take() {
                match tokio::time::timeout_at(
                    tokio::time::Instant::from_std(settlement_end),
                    &mut monitor,
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => {
                        remember(&mut self.first, &self.failure, Err(Error::Close));
                    }
                    Err(_) => {
                        remember(&mut self.first, &self.failure, Err(Error::Deadline));
                        monitor.abort();
                        let _ = monitor.await;
                    }
                }
            }
            // No nonconsuming PID monitor remains when this sole owner consumes wait.
            if let Some(group) = &mut self.group {
                let (retired, reaped) = retire(group, settlement_end, Duration::from_secs(5)).await;
                self.retired = retired;
                if !retired {
                    remember(&mut self.first, &self.failure, Err(Error::Close));
                }
                match reaped {
                    Ok(status) => {
                        self.reaped = true;
                        self.status = status.code().or_else(|| status.signal().map(|n| -n));
                        if let Some(status) = self.status.filter(|n| *n != 0) {
                            remember(&mut self.first, &self.failure, Err(Error::Child(status)));
                        }
                        if self.status.is_none() {
                            remember(&mut self.first, &self.failure, Err(Error::Close));
                        }
                    }
                    Err(error) => {
                        remember(&mut self.first, &self.failure, Err(error));
                    }
                }
            }
            if let Some(mut reader) = self.reader.take() {
                let end = (Instant::now() + Duration::from_secs(1)).min(settlement_end);
                match tokio::time::timeout_at(tokio::time::Instant::from_std(end), &mut reader)
                    .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => {
                        self.facts
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .evidence_uncertain();
                        remember(&mut self.first, &self.failure, Err(Error::Close));
                    }
                    Err(_) => {
                        self.facts
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .evidence_uncertain();
                        remember(&mut self.first, &self.failure, Err(Error::Close));
                        self.stop_reader.store(true, Ordering::SeqCst);
                        let end = (Instant::now() + Duration::from_secs(5)).min(settlement_end);
                        if tokio::time::timeout_at(tokio::time::Instant::from_std(end), &mut reader)
                            .await
                            .is_err()
                        {
                            reader.abort();
                            let _ = reader.await;
                        }
                    }
                }
                self.facts
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .joined = true;
            }
            {
                let mut data = self
                    .facts
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let mut owned = self
                    .owned
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                owned.finish(&mut data, &self.failure);
                if let Some(error) = data.first {
                    remember(&mut self.first, &self.failure, Err(error));
                }
            }
            if let Some(observer) = &mut self.qmp {
                let finished = if keep_handoff {
                    observer.finish_before(settlement_end).await
                } else {
                    observer.finish().await
                };
                if let Err(error) = finished {
                    remember(&mut self.first, &self.failure, Err(error));
                }
            }
            if let Some(error) = self.first {
                control.record(error);
            }
            if keep_handoff {
                let mut handoff = self.handoff.take();
                let sealed = (|| {
                    let facts = self.facts.lock().map_err(|_| Error::Close)?;
                    if self.first.is_some()
                        || !self.reaped
                        || !self.retired
                        || self.status != Some(0)
                        || !facts.joined
                        || !facts.eof.iter().all(|value| *value)
                        || !facts.closed.iter().all(|value| *value)
                        || !self.authenticated.load(Ordering::SeqCst)
                        || !self.cloud_init
                        || self.qmp.is_none()
                    {
                        return Err(Error::Close);
                    }
                    drop(facts);
                    handoff.as_mut().ok_or(Error::Changed)?.seal(self, control)
                })();
                self.handoff = handoff;
                if let Err(error) = sealed {
                    control.record(error);
                }
                remember(&mut self.first, &self.failure, sealed);
            }
            if let Some(disk) = &mut self.overlay {
                let checked = disk.check(control);
                if let Err(error) = checked {
                    control.record(error);
                }
                remember(&mut self.first, &self.failure, checked);
                remember(&mut self.first, &self.failure, disk.finish(control));
            }
            for original in &mut self.originals {
                remember(&mut self.first, &self.failure, original.finish(control));
            }
            remember(&mut self.first, &self.failure, control.check());
            remember(&mut self.first, &self.failure, live(self.end));
            let data = self
                .facts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !self.authenticated.load(Ordering::SeqCst)
                || !self.cloud_init
                || !self.reaped
                || !self.retired
                || !data.joined
                || !data.eof.iter().all(|v| *v)
                || !data.closed.iter().all(|v| *v)
            {
                remember(&mut self.first, &self.failure, Err(Error::Close));
            }
            if self.qmp.is_none() {
                remember(&mut self.first, &self.failure, Err(Error::Close));
            }
        }
        if !keep_handoff && let Some(mut handoff) = self.handoff.take() {
            let finished = handoff.finish(control);
            remember(&mut self.first, &self.failure, finished);
        }
        let observed = *self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(error) = observed.or(self.first) {
            control.record(error);
            Err(error)
        } else {
            Ok(self.diagnostics())
        }
    }
    /// Complete bounded stderr evidence is passive and only describes failed startup.
    pub fn startup_diagnostic(&self) -> Option<Value> {
        if !self.closed
            || !self.reaped
            || self.authenticated.load(Ordering::SeqCst)
            || Instant::now() >= self.end
        {
            return None;
        }
        let status = self.status.filter(|value| *value != 0)?;
        let data = self.facts.lock().ok()?;
        if data.uncertain
            || !data.joined
            || !data.eof.iter().all(|value| *value)
            || !data.closed.iter().all(|value| *value)
        {
            return None;
        }
        let stderr = data.stderr.as_ref()?;
        Some(
            json!({"epoch":if self.restricted {"restricted"} else {"preparation"},
            "rawStatus":status,"stderrClass":stderr.class.name(),
            "stderrSha256":stderr.sha256}),
        )
    }
    pub fn diagnostics(&self) -> Value {
        let data = self
            .facts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let qmp = self.qmp.as_ref().map(qmp::Observer::diagnostics);
        json!({"qemuReaped":self.reaped,"qemuExitedZero":self.status==Some(0),
            "qemuReturnCode":self.status,"groupRetired":self.retired,
            "authenticatedSsh":self.authenticated.load(Ordering::SeqCst),
            "drainerCompleted":data.joined&&data.completed&&data.first.is_none()
                &&data.eof.iter().all(|v|*v)
                &&data.closed.iter().all(|v|*v),"cloudInitPassed":self.cloud_init,
            "stdoutBytes":data.bytes[0],"stderrBytes":data.bytes[1],
            "stdoutEof":data.eof[0],"stderrEof":data.eof[1],
            "allReturnedPipesAndLogsClosed":data.closed.iter().all(|v|*v),
            "qmp":qmp,"closeReturned":self.closed&&self.first.is_none()
                &&self.failure.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_none()})
    }
}

fn cloud_errors(row: &Value) -> Result<bool> {
    let errors = row["errors"].as_array().ok_or(Error::Refused)?;
    let recoverable = row["recoverable_errors"]
        .as_object()
        .ok_or(Error::Refused)?;
    if recoverable.len() + 1 > 17 {
        return Err(Error::Refused);
    }
    let mut count = 0;
    for group in std::iter::once(errors).chain(
        recoverable
            .values()
            .map(|v| v.as_array())
            .collect::<Option<Vec<_>>>()
            .ok_or(Error::Refused)?,
    ) {
        if group.len() > 128 {
            return Err(Error::Refused);
        }
        for value in group {
            let message = value.as_str().ok_or(Error::Refused)?;
            if message.chars().count() > 4096 {
                return Err(Error::Refused);
            }
            count += 1;
        }
    }
    if count > 512 {
        return Err(Error::Refused);
    }
    Ok(count == 0)
}
fn cloud_passed(bytes: &[u8], status: i32) -> Result<bool> {
    if bytes.len() > 65536 {
        return Err(Error::Refused);
    }
    let row = super::decode(bytes)?;
    let running = ["not started", "running", "done", "disabled"];
    let label = row["status"].as_str().ok_or(Error::Refused)?;
    let extended = row["extended_status"].as_str().ok_or(Error::Refused)?;
    if !running.contains(&label) && label != "error" {
        return Err(Error::Refused);
    }
    if !running.contains(&extended)
        && !matches!(extended, "degraded done" | "degraded running")
        && !running
            .iter()
            .any(|name| extended == format!("error - {name}"))
    {
        return Err(Error::Refused);
    }
    let stages = ["init-local", "init", "modules-config", "modules-final"];
    if row.get("stage").is_none()
        || (!row["stage"].is_null()
            && row["stage"]
                .as_str()
                .is_none_or(|value| !stages.contains(&value)))
    {
        return Err(Error::Refused);
    }
    let mut healthy = cloud_errors(&row)?;
    let mut count = 0;
    for stage in stages {
        if let Some(value) = row.get(stage) {
            healthy &= cloud_errors(value)?;
            count += 1;
        }
    }
    Ok(status == 0
        && label == "done"
        && extended == "done"
        && row["stage"].is_null()
        && count == 4
        && healthy)
}

#[cfg(test)]
mod tests {
    use super::*;
    mod reader_tests {
        use super::*;
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        use std::sync::atomic::AtomicU64;

        static NEXT: AtomicU64 = AtomicU64::new(0);
        struct Fixture {
            vm: Vm,
            control: Control,
            writers: [Option<File>; 2],
            root: PathBuf,
        }
        impl Fixture {
            fn new() -> Self {
                let root = Path::new("/tmp").canonicalize().unwrap().join(format!(
                    "qemu-reader-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::SeqCst)
                ));
                fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
                fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
                let mut control = Control::new(Instant::now()).unwrap();
                let mut vm = Vm::epoch(
                    root.clone(),
                    2222,
                    Instant::now() + Duration::from_secs(10),
                    "a".repeat(32),
                    true,
                );
                let mut writers = [None, None];
                for (index, writer) in writers.iter_mut().enumerate() {
                    let (read, write) = nix::unistd::pipe().unwrap();
                    vm.owned.lock().unwrap().pipes[index] =
                        Some(Pipe::new(File::from(read), &mut control).unwrap());
                    *writer = Some(File::from(write));
                    vm.owned.lock().unwrap().logs[index] = Some(
                        files::private_file(&root.join(format!("stream-{index}")), &mut control)
                            .unwrap(),
                    );
                }
                let arguments = qemu_arguments(
                    &root,
                    2222,
                    &"a".repeat(32),
                    "/usr/bin/prlimit".into(),
                    "/usr/bin/qemu-system-x86_64".into(),
                    true,
                    10,
                )
                .unwrap();
                vm.startup_profile = StartupProfile::new(&arguments, &root);
                Self {
                    vm,
                    control,
                    writers,
                    root,
                }
            }
            fn write(&mut self, index: usize, bytes: &[u8]) {
                self.writers[index]
                    .as_mut()
                    .unwrap()
                    .write_all(bytes)
                    .unwrap();
            }
            fn close_writer(&mut self, index: usize) {
                if let Some(writer) = self.writers[index].take() {
                    close(writer).unwrap();
                }
            }
            fn pump(&mut self, limits: [usize; 2], closing: bool) -> Result<bool> {
                self.vm.closing.store(closing, Ordering::SeqCst);
                self.vm.owned.lock().unwrap().pump(
                    &mut self.vm.facts.lock().unwrap(),
                    &self.vm.failure,
                    &self.vm.health,
                    &self.vm.closing,
                    limits,
                    self.vm.end,
                )
            }
            async fn drain(&mut self, limits: [usize; 2], closing: bool) -> Result<bool> {
                // Each bounded poll may consume payload without observing the following EOF.
                // The same immutable fixture end still fences every subsequent poll.
                loop {
                    match self.pump(limits, closing) {
                        Ok(false) => tokio::task::yield_now().await,
                        result => return result,
                    }
                }
            }
            fn settle_streams(&mut self) {
                let mut facts = self.vm.facts.lock().unwrap();
                let mut owned = self.vm.owned.lock().unwrap();
                if !facts.uncertain && facts.eof.iter().all(|value| *value) {
                    facts.stderr = owned
                        .stderr_observation(&facts, self.vm.startup_profile.as_ref(), self.vm.end)
                        .ok();
                }
                owned.finish(&mut facts, &self.vm.failure);
            }
            async fn finish(mut self) {
                for index in 0..2 {
                    self.close_writer(index);
                }
                if let Some(mut task) = self.vm.reader.take() {
                    let end = tokio::time::Instant::from_std(self.vm.end);
                    if tokio::time::timeout_at(end, &mut task).await.is_err() {
                        task.abort();
                        let _ = task.await;
                    }
                }
                {
                    let mut facts = self.vm.facts.lock().unwrap();
                    self.vm
                        .owned
                        .lock()
                        .unwrap()
                        .finish(&mut facts, &self.vm.failure);
                }
                if let Some(first) = *self.vm.failure.lock().unwrap() {
                    self.control.record(first);
                }
                self.control.settle();
                let _ = self.control.finish();
                let logs_closed = self
                    .vm
                    .owned
                    .lock()
                    .unwrap()
                    .logs
                    .iter()
                    .all(Option::is_none);
                let pipes_closed = self
                    .vm
                    .owned
                    .lock()
                    .unwrap()
                    .pipes
                    .iter()
                    .flatten()
                    .all(|pipe| pipe.closed);
                let removed = fs::remove_dir_all(self.root);
                assert!(logs_closed && pipes_closed);
                assert!(removed.is_ok());
            }
        }

        #[tokio::test]
        async fn stdout_eof_latches_before_delayed_peer_and_keeps_complete_stderr_negative() {
            let mut fixture = Fixture::new();
            let end = fixture.vm.end;
            fixture.vm.start_readers();
            fixture.close_writer(0);
            let fence = (Instant::now() + Duration::from_secs(3)).min(end);
            while fixture.vm.health.load(Ordering::SeqCst) && Instant::now() < fence {
                tokio::task::yield_now().await;
            }
            let refused_before_peer = *fixture.vm.failure.lock().unwrap();
            let bytes = [b'x'; 265];
            fixture.write(1, &bytes);
            fixture.close_writer(1);
            let mut task = fixture.vm.reader.take().unwrap();
            let joined =
                tokio::time::timeout_at(tokio::time::Instant::from_std(end), &mut task).await;
            if joined.is_err() {
                task.abort();
                let _ = task.await;
            }
            let joined = matches!(joined, Ok(Ok(())));
            fixture.vm.facts.lock().unwrap().joined = joined;
            // Only the enclosing process status is synthetic in this pipe-owner test.
            fixture.vm.closed = true;
            fixture.vm.reaped = true;
            fixture.vm.status = Some(1);
            let diagnostic = fixture.vm.startup_diagnostic();
            let physical = {
                let facts = fixture.vm.facts.lock().unwrap();
                (facts.eof, facts.closed, facts.bytes, facts.completed)
            };
            let before = fixture.vm.diagnostics();
            fixture.finish().await;
            assert_eq!(refused_before_peer, Some(Error::Refused));
            assert!(joined);
            assert_eq!(physical, ([true; 2], [true; 4], [0, 265], false));
            assert_eq!(before["drainerCompleted"], false);
            assert_eq!(before["closeReturned"], false);
            let diagnostic = diagnostic.unwrap();
            assert_eq!(diagnostic["rawStatus"], 1);
            assert_eq!(diagnostic["stderrClass"], "unclassified");
            assert_eq!(
                diagnostic["stderrSha256"],
                files::hex(&Sha256::digest(bytes))
            );
        }

        #[tokio::test]
        async fn both_eof_orders_drain_peer_and_never_replace_a_prior_error() {
            for first in 0..2 {
                let mut fixture = Fixture::new();
                *fixture.vm.failure.lock().unwrap() = Some(Error::Io);
                fixture.close_writer(first);
                let incomplete = fixture.pump([STREAM_MAX; 2], false);
                fixture.write(1 - first, b"peer evidence");
                fixture.close_writer(1 - first);
                let complete = fixture.drain([STREAM_MAX; 2], false).await;
                fixture.settle_streams();
                let (eof, closed) = {
                    let state = fixture.vm.facts.lock().unwrap();
                    (state.eof, state.closed)
                };
                let first_error = *fixture.vm.failure.lock().unwrap();
                fixture.finish().await;
                assert_eq!(incomplete, Ok(false));
                assert_eq!(complete, Ok(true));
                assert_eq!(eof, [true; 2]);
                assert_eq!(closed, [true; 4]);
                assert_eq!(first_error, Some(Error::Io));
            }
        }

        #[tokio::test]
        async fn bounded_leg_overflow_and_real_log_write_error_still_settle_the_peer() {
            for write_error in [false, true] {
                let mut fixture = Fixture::new();
                if write_error {
                    let original = fixture.vm.owned.lock().unwrap().logs[0].take().unwrap();
                    close(original).unwrap();
                    let read_only = File::open(fixture.root.join("stream-0")).unwrap();
                    fixture.vm.owned.lock().unwrap().logs[0] = Some(read_only);
                }
                fixture.write(0, b"12345");
                fixture.close_writer(0);
                fixture.write(1, b"bounded peer");
                fixture.close_writer(1);
                let result = fixture
                    .drain(
                        [if write_error { STREAM_MAX } else { 4 }, STREAM_MAX],
                        false,
                    )
                    .await;
                fixture.settle_streams();
                let observed = {
                    let state = fixture.vm.facts.lock().unwrap();
                    (
                        state.faulted[0],
                        state.eof[1],
                        state.bytes[1],
                        state.uncertain,
                        state.stderr.is_none(),
                        state.closed,
                    )
                };
                fixture.finish().await;
                assert_eq!(result, Ok(true));
                assert_eq!(observed, (true, true, 12, true, true, [true; 4]));
            }
        }

        #[tokio::test]
        async fn expected_closing_and_incomplete_or_late_evidence_keep_their_distinctions() {
            let mut fixture = Fixture::new();
            fixture.close_writer(0);
            let incomplete = fixture.pump([STREAM_MAX; 2], false);
            let unavailable = fixture
                .vm
                .owned
                .lock()
                .unwrap()
                .stderr_observation(&fixture.vm.facts.lock().unwrap(), None, fixture.vm.end)
                .is_err();
            fixture.vm.end = Instant::now();
            let late = fixture.pump([STREAM_MAX; 2], false);
            fixture.finish().await;
            assert_eq!(incomplete, Ok(false));
            assert!(unavailable);
            assert_eq!(late, Err(Error::Deadline));
            let mut fixture = Fixture::new();
            fixture.close_writer(0);
            fixture.close_writer(1);
            let complete = fixture.pump([STREAM_MAX; 2], true);
            fixture.settle_streams();
            let first = *fixture.vm.failure.lock().unwrap();
            fixture.vm.closed = true;
            fixture.vm.reaped = true;
            fixture.vm.facts.lock().unwrap().joined = true;
            fixture.vm.status = Some(1);
            let before_ssh = fixture.vm.startup_diagnostic();
            fixture.vm.status = Some(0);
            let zero = fixture.vm.startup_diagnostic();
            fixture.vm.status = None;
            let unknown = fixture.vm.startup_diagnostic();
            fixture.vm.status = Some(1);
            fixture.vm.authenticated.store(true, Ordering::SeqCst);
            let admitted = fixture.vm.startup_diagnostic();
            fixture.finish().await;
            assert_eq!(complete, Ok(true));
            assert_eq!(first, None);
            assert!(before_ssh.is_some());
            assert!(zero.is_none() && unknown.is_none() && admitted.is_none());
        }

        #[test]
        fn startup_classes_require_the_full_bound_records_not_substrings() {
            let work = Path::new("/tmp/owned");
            let arguments = qemu_arguments(
                work,
                2222,
                &"a".repeat(32),
                "/usr/bin/prlimit".into(),
                "/usr/bin/qemu-system-x86_64".into(),
                true,
                10,
            )
            .unwrap();
            let profile = StartupProfile::new(&arguments, work).unwrap();
            let wait = concat!(
                "qemu-system-x86_64: -qmp unix:/tmp/owned/qmp-restricted.sock,server=on,wait=on: ",
                "info: QEMU waiting for connection on: ",
                "disconnected:unix:/tmp/owned/qmp-restricted.sock,server=on\n"
            );
            let network = concat!(
                "qemu-system-x86_64: -netdev user,id=net0,restrict=on,ipv4=on,ipv6=off,",
                "hostfwd=tcp:127.0.0.1:2222-:22: Could not set up host forwarding rule ",
                "'tcp:127.0.0.1:2222-:22'\n"
            );
            let literal = format!("{wait}{network}");
            assert_eq!(
                profile.classify(literal.as_bytes()).name(),
                "host-forward-setup"
            );
            let block = concat!(
                "qemu-system-x86_64: -blockdev {\"driver\":\"qcow2\",\"file\":{",
                "\"driver\":\"file\",\"filename\":\"/tmp/owned/guest.qcow2\"},",
                "\"node-name\":\"rootdisk\"}: "
            );
            for (suffix, expected) in [
                (
                    concat!(
                        "Could not open backing file: Could not open ",
                        "'/tmp/owned/base.qcow2': No such file or directory\n"
                    ),
                    "backing-file-missing",
                ),
                ("Image is not in qcow2 format\n", "qcow2-format"),
                (
                    concat!(
                        "Failed to get \"write\" lock\n",
                        "Is another process using the image [/tmp/owned/guest.qcow2]?\n"
                    ),
                    "image-write-lock",
                ),
            ] {
                let record = format!("{wait}{block}{suffix}");
                assert_eq!(profile.classify(record.as_bytes()).name(), expected);
            }
            // The recovered hosted tail is fixed; all paths here remain contextual fixtures.
            let permission_tail = concat!(
                "Could not access KVM kernel module: Permission denied\n",
                "qemu-system-x86_64: failed to initialize kvm: Permission denied\n"
            );
            assert_eq!(permission_tail.len(), 118);
            let kvm = format!("{wait}{permission_tail}");
            assert_eq!(profile.classify(kvm.as_bytes()).name(), "kvm-access");
            for neighbor in [
                kvm.replace("Permission denied", "Operation not permitted"),
                kvm.replace("Permission denied", "No such file or directory"),
                kvm.replace(
                    "Could not access KVM kernel module: Permission denied\n",
                    "",
                ),
                kvm.replace(
                    "qemu-system-x86_64: failed to initialize kvm: Permission denied\n",
                    "",
                ),
                permission_tail.to_owned(),
            ] {
                assert_eq!(profile.classify(neighbor.as_bytes()).name(), "unclassified");
            }
            for (kind, record) in &profile.records {
                assert_eq!(profile.classify(record.as_bytes()).name(), kind.name());
                for altered in [
                    format!("{record}private neighbor\n"),
                    record.replace("/tmp/owned", "/tmp/foreign"),
                    record.replace("qemu-system-x86_64:", "foreign:"),
                    record.trim_end().to_owned(),
                ] {
                    assert_eq!(profile.classify(altered.as_bytes()).name(), "unclassified");
                }
            }
            let wrong_port = literal.replace("2222", "2223");
            assert_eq!(
                profile.classify(wrong_port.as_bytes()).name(),
                "unclassified"
            );
            assert_eq!(
                profile
                    .classify(b"Could not set up host forwarding rule")
                    .name(),
                "unclassified"
            );
        }
    }

    #[tokio::test]
    async fn incomplete_preparation_epoch_cannot_claim_closure_or_fresh_health() {
        let mut control = Control::new(Instant::now()).unwrap();
        let mut vm = Vm::epoch(
            PathBuf::from("/tmp/inert-unstarted"),
            2222,
            control.end(),
            "a".repeat(32),
            false,
        );
        let old_health = vm.health();
        assert_eq!(vm.finish_epoch(&mut control, true).await, Err(Error::Close));
        assert!(Arc::ptr_eq(&old_health, &vm.health));
        assert!(!old_health.load(Ordering::SeqCst));
        let facts = vm.diagnostics();
        assert_eq!(facts["qemuReaped"], false);
        assert_eq!(facts["qemuExitedZero"], false);
        assert_eq!(facts["qemuReturnCode"], Value::Null);
        assert_eq!(facts["closeReturned"], false);
        control.settle();
        assert_eq!(control.finish(), Err(Error::Close));
    }
    #[test]
    fn two_epochs_share_each_original_stream_allowance() {
        assert_eq!(remaining_streams([0, STREAM_MAX]), Ok([STREAM_MAX, 0]));
        assert_eq!(
            remaining_streams([1024, 8192]),
            Ok([STREAM_MAX - 1024, STREAM_MAX - 8192])
        );
        assert_eq!(remaining_streams([STREAM_MAX + 1, 0]), Err(Error::Refused));
        assert_eq!(remaining_streams([0, usize::MAX]), Err(Error::Refused));
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn fifo_disk_is_opened_nonblocking_and_refused_before_vm_use() {
        use std::os::unix::fs::PermissionsExt;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "maintain-disk-fifo-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let mut control = Control::new(Instant::now()).unwrap();
        let mut parent = None;
        let mut keeper = None;
        let mut disk = None;
        let attempted: Result<()> = (|| {
            parent = Some(File::open(&root).map_err(|_| Error::Io)?);
            rustix::fs::mkfifoat(
                parent.as_ref().ok_or(Error::Close)?,
                "guest.qcow2",
                rustix::fs::Mode::from_bits_truncate(0o600),
            )
            .map_err(|_| Error::Io)?;
            // Keep both ends open so a regression missing NONBLOCK cannot hang
            // this test. The separate observed kernel flag still detects it.
            keeper = Some(
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(
                        (rustix::fs::OFlags::NONBLOCK
                            | rustix::fs::OFlags::NOFOLLOW
                            | rustix::fs::OFlags::CLOEXEC)
                            .bits() as i32,
                    )
                    .open(root.join("guest.qcow2"))
                    .map_err(|_| Error::Io)?,
            );
            DISK_OPEN_FLAGS.set(None);
            disk = Some(MutableDisk::open(&root.join("guest.qcow2"), &mut control)?);
            Ok(())
        })();
        let flags = DISK_OPEN_FLAGS.get();
        if let Err(error) = attempted {
            control.record(error);
        }
        let disk_close = disk.as_mut().map(|disk| disk.finish(&mut control));
        let mut closed = Ok(());
        for file in [keeper.take(), parent.take()].into_iter().flatten() {
            let result = close(file);
            closed = closed.and(result);
        }
        control.settle();
        let finished = control.finish();
        let removed = fs::remove_dir_all(root);
        assert_eq!(attempted, Err(Error::Changed));
        assert!(flags.is_some_and(|flags| flags.contains(rustix::fs::OFlags::NONBLOCK)));
        assert!(disk_close.is_none());
        assert_eq!(closed, Ok(()));
        assert_eq!(finished, Err(Error::Changed));
        assert!(removed.is_ok());
    }
    #[test]
    fn exact_pin_rejects_cross_version_and_untrusted_scheme() {
        let original = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../security/vm/debian-cloud.json"
        ));
        assert!(pin(original).is_ok());
        for changed in [
            String::from_utf8_lossy(original).replace("https:", "http:"),
            String::from_utf8_lossy(original)
                .replace("debian-13-genericcloud", "debian-12-genericcloud"),
        ] {
            assert!(pin(changed.as_bytes()).is_err());
        }
    }
    #[test]
    fn ssh_is_single_fixed_loopback_destination_without_ambient_policy() {
        let args =
            ssh_arguments(Path::new("/tmp/owned"), 2222, &["true; 'quoted'".into()]).unwrap();
        assert!(args.iter().any(|s| s == "fixture@127.0.0.1"));
        assert!(args.iter().any(|s| s == "StrictHostKeyChecking=yes"));
        assert!(args.iter().any(|s| s == "GlobalKnownHostsFile=/dev/null"));
        assert_eq!(
            args.last(),
            Some(&OsString::from("'true; '\\''quoted'\\'''"))
        );
        assert!(ssh_arguments(Path::new("/tmp/owned"), 22, &["true".into()]).is_err());
    }
    #[test]
    fn ipv4_provider_and_ssh_join_the_same_private_forward_and_device() {
        let work = Path::new("/tmp/owned, fixture");
        for port in [2222, 65535] {
            let args = qemu_arguments(
                work,
                port,
                "0123456789abcdef0123456789abcdef",
                "/usr/bin/prlimit".into(),
                "/usr/bin/qemu-system-x86_64".into(),
                false,
                1800,
            )
            .unwrap();
            let args = args.iter().map(|v| v.to_str().unwrap()).collect::<Vec<_>>();
            let networks = args
                .windows(2)
                .filter(|pair| pair[0] == "-netdev")
                .collect::<Vec<_>>();
            assert_eq!(networks.len(), 1);
            let fields = networks[0][1].split(',').collect::<Vec<_>>();
            assert_eq!(fields.len(), 5);
            assert_eq!(fields[0], "user");
            let options = fields[1..]
                .iter()
                .map(|value| value.split_once('=').unwrap())
                .collect::<std::collections::BTreeMap<_, _>>();
            assert_eq!(options.len(), 4);
            assert_eq!(options["ipv4"], "on");
            assert_eq!(options["ipv6"], "off");
            assert_eq!(options["hostfwd"], format!("tcp:127.0.0.1:{port}-:22"));
            let devices = args
                .windows(2)
                .filter(|pair| pair[0] == "-device")
                .map(|pair| pair[1])
                .filter(|device| device.starts_with("virtio-net-pci"))
                .collect::<Vec<_>>();
            assert_eq!(
                devices,
                [format!("virtio-net-pci,netdev={}", options["id"])]
            );
            let ssh = ssh_arguments(work, port, &["true".into()]).unwrap();
            let forwarded = ssh
                .windows(2)
                .filter(|pair| pair[0] == "-p")
                .map(|pair| &pair[1])
                .collect::<Vec<_>>();
            assert_eq!(forwarded, [&OsString::from(port.to_string())]);
            assert!(ssh.iter().any(|value| value == "fixture@127.0.0.1"));
        }
    }
    #[test]
    fn restricted_epoch_keeps_the_same_forward_disk_and_original_watchdog() {
        let work = Path::new("/tmp/owned");
        let run = "0123456789abcdef0123456789abcdef";
        let arguments = |restricted, seconds| {
            qemu_arguments(
                work,
                2222,
                run,
                "/usr/bin/prlimit".into(),
                "/usr/bin/qemu-system-x86_64".into(),
                restricted,
                seconds,
            )
        };
        let first = arguments(false, 1800).unwrap();
        let second = arguments(true, 721).unwrap();
        let differences = first
            .iter()
            .zip(&second)
            .filter(|(a, b)| a != b)
            .collect::<Vec<_>>();
        assert_eq!(differences.len(), 3);
        assert!(second.iter().any(|v| v == "721s"));
        assert!(second.iter().any(|v| v
            == concat!(
                "user,id=net0,restrict=on,ipv4=on,ipv6=off,",
                "hostfwd=tcp:127.0.0.1:2222-:22"
            )));
        assert!(
            second
                .iter()
                .any(|v| v == "unix:/tmp/owned/qmp-restricted.sock,server=on,wait=on")
        );
        for seconds in [0, 1801, u64::MAX] {
            assert!(arguments(true, seconds).is_err());
        }
        assert!(arguments(true, 1).is_ok());
        let now = Instant::now();
        let end = now + Duration::from_secs(1800);
        assert_eq!(
            watchdog_seconds(end, now + Duration::from_millis(1001)),
            Ok(1798)
        );
        assert_eq!(
            watchdog_seconds(end, end - Duration::from_millis(999)),
            Err(Error::Deadline)
        );
        assert_eq!(watchdog_seconds(end, end), Err(Error::Deadline));
    }
    #[test]
    fn restricted_socket_retains_the_original_exact_path_bound() {
        let name = "qmp-restricted.sock";
        for (length, accepted) in [(100usize, true), (101, false)] {
            let root = PathBuf::from(format!("/{}", "a".repeat(length - name.len() - 2)));
            assert_eq!(root.join(name).as_os_str().as_encoded_bytes().len(), length);
            let arguments = qemu_arguments(
                &root,
                2222,
                "0123456789abcdef0123456789abcdef",
                "/usr/bin/prlimit".into(),
                "/usr/bin/qemu-system-x86_64".into(),
                true,
                30,
            );
            assert_eq!(arguments.is_ok(), accepted);
        }
    }
    #[test]
    fn cloud_completion_requires_all_four_clean_stages_and_actual_zero() {
        let clean = json!({"errors":[],"recoverable_errors":{}});
        let mut row = json!({"status":"done","extended_status":"done","stage":null,
            "errors":[],"recoverable_errors":{},"init-local":clean,"init":clean,
            "modules-config":clean,"modules-final":clean});
        assert_eq!(
            cloud_passed(&serde_json::to_vec(&row).unwrap(), 0),
            Ok(true)
        );
        assert_eq!(
            cloud_passed(&serde_json::to_vec(&row).unwrap(), 2),
            Ok(false)
        );
        row["modules-final"]["errors"] = json!(["INERT_SECRET"]);
        assert_eq!(
            cloud_passed(&serde_json::to_vec(&row).unwrap(), 0),
            Ok(false)
        );
        row.as_object_mut().unwrap().remove("stage");
        assert_eq!(
            cloud_passed(&serde_json::to_vec(&row).unwrap(), 0),
            Err(Error::Refused)
        );
    }
}
