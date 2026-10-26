//! One bounded, passive QMP observer for the keeper's original private socket.
//!
//! This sends only capabilities negotiation. Missing events prove neither an
//! event-free interval nor visibility before negotiation. Failed SDK connect
//! attempts have opaque internal closes; only the returned stream and original
//! parent receive checked-close facts. Other connect/conversion errors refuse.

use super::super::{Error, Result};
use serde_json::{Value, json};
use std::{
    ffi::OsStr,
    fs::{self, File, Metadata},
    io::{self, Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt},
        net::UnixStream,
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{net::UnixStream as AsyncStream, task::JoinHandle, time::timeout_at};

const MAX_BYTES: usize = 65536;
const MAX_MESSAGES: usize = 128;
const MAX_LINE: usize = 8192;
const MAX_PATH: usize = 100;
const MAX_VERSION: u64 = 65535;
const MAX_CAPABILITIES: usize = 32;
const CONNECT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(100);
const CLOSE: Duration = Duration::from_secs(1);
const CAPABILITIES: &[u8] = b"{\"execute\":\"qmp_capabilities\",\"id\":\"capabilities\"}\r\n";
const REASONS: &[&str] = &[
    "none",
    "host-error",
    "host-qmp-quit",
    "host-qmp-system-reset",
    "host-signal",
    "host-ui",
    "guest-shutdown",
    "guest-reset",
    "guest-panic",
    "subsystem-reset",
    "snapshot-load",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Code {
    Path,
    Parent,
    Socket,
    Namespace,
    Start,
    Deadline,
    ConnectIncomplete,
    Conversion,
    Greeting,
    Message,
    MessageLimit,
    Event,
    Reason,
    Guest,
    Negotiation,
    ByteLimit,
    LineLimit,
    EofIncomplete,
    ObservationIncomplete,
    CleanupIncomplete,
    Json,
    Io,
    Close,
    Observer,
}
impl Code {
    fn name(self) -> &'static str {
        match self {
            Self::Path => "vm_qmp_path",
            Self::Parent => "vm_qmp_parent",
            Self::Socket => "vm_qmp_socket",
            Self::Namespace => "vm_qmp_namespace",
            Self::Start => "vm_qmp_start",
            Self::Deadline => "vm_qmp_deadline",
            Self::ConnectIncomplete => "vm_qmp_connect_incomplete",
            Self::Conversion => "vm_qmp_conversion",
            Self::Greeting => "vm_qmp_greeting",
            Self::Message => "vm_qmp_message",
            Self::MessageLimit => "vm_qmp_message_limit",
            Self::Event => "vm_qmp_event",
            Self::Reason => "vm_qmp_reason",
            Self::Guest => "vm_qmp_guest",
            Self::Negotiation => "vm_qmp_negotiation",
            Self::ByteLimit => "vm_qmp_byte_limit",
            Self::LineLimit => "vm_qmp_line_limit",
            Self::EofIncomplete => "vm_qmp_eof_incomplete",
            Self::ObservationIncomplete => "vm_qmp_observation_incomplete",
            Self::CleanupIncomplete => "vm_qmp_cleanup_incomplete",
            Self::Json => "vm_qmp_json",
            Self::Io => "vm_qmp_io",
            Self::Close => "vm_qmp_close",
            Self::Observer => "vm_qmp_observer",
        }
    }
    fn error(self) -> Error {
        match self {
            Self::Deadline => Error::Deadline,
            Self::Io | Self::Conversion => Error::Io,
            Self::Close | Self::CleanupIncomplete => Error::Close,
            _ => Error::Refused,
        }
    }
}
type Observed<T> = std::result::Result<T, Code>;
type FailureSlot = Arc<Mutex<Option<Error>>>;

fn record_error(failure: &FailureSlot, error: Error) {
    let mut first = match failure.lock() {
        Ok(first) => first,
        Err(poisoned) => poisoned.into_inner(),
    };
    first.get_or_insert(error);
}
fn unhealthy(health: &AtomicBool, failure: &FailureSlot, error: Error) {
    // Mutex publication precedes the release-store consumed by Runner. The
    // slot is a passive first observation, never another deadline/Owner.
    record_error(failure, error);
    health.store(false, Ordering::Release);
}

fn live(now: Instant, end: Instant) -> Observed<()> {
    if now < end {
        Ok(())
    } else {
        Err(Code::Deadline)
    }
}
fn clipped(now: Instant, duration: Duration, end: Instant) -> Instant {
    now.checked_add(duration).unwrap_or(end).min(end)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    gid: u32,
}
impl From<&Metadata> for Stamp {
    fn from(value: &Metadata) -> Self {
        Self {
            device: value.dev(),
            inode: value.ino(),
            mode: value.mode(),
            uid: value.uid(),
            gid: value.gid(),
        }
    }
}

#[derive(Default)]
struct Facts {
    connected: bool,
    greeting: bool,
    negotiated: bool,
    eof: bool,
    bytes: usize,
    messages: usize,
    withheld: usize,
    events: Vec<Value>,
    failure: Option<Code>,
    stream_close_attempted: bool,
    stream_closed: bool,
    parent_close_attempted: bool,
    parent_closed: bool,
    task_joined: bool,
    forced_stop: bool,
    cleanup_incomplete: bool,
}
impl Facts {
    fn fail(&mut self, code: Code) {
        // The Python reference overwrites its code on incomplete cleanup.
        // This closed Rust shape preserves first cause and exposes that debt
        // separately; neither shape permits incomplete cleanup to pass.
        if code == Code::CleanupIncomplete {
            self.cleanup_incomplete = true;
        }
        self.failure.get_or_insert(code);
    }
    fn message(&mut self, raw: &[u8]) -> Observed<bool> {
        self.messages += 1;
        if self.messages > MAX_MESSAGES {
            return Err(Code::MessageLimit);
        }
        let message = super::super::decode(raw).map_err(|_| Code::Json)?;
        let object = message.as_object().ok_or(Code::Message)?;
        if !self.greeting {
            if object.len() != 1 || !object.contains_key("QMP") {
                return Err(Code::Greeting);
            }
            let hello = message["QMP"].as_object().ok_or(Code::Greeting)?;
            let qemu = message["QMP"]["version"]["qemu"]
                .as_object()
                .ok_or(Code::Greeting)?;
            for name in ["major", "minor", "micro"] {
                if qemu
                    .get(name)
                    .and_then(Value::as_u64)
                    .is_none_or(|value| value > MAX_VERSION)
                {
                    return Err(Code::Greeting);
                }
            }
            if hello
                .get("capabilities")
                .and_then(Value::as_array)
                .is_none_or(|values| values.len() > MAX_CAPABILITIES)
            {
                return Err(Code::Greeting);
            }
            return Ok(true);
        }
        if object.contains_key("event") {
            let event = message["event"].as_str().ok_or(Code::Event)?;
            if !matches!(event, "SHUTDOWN" | "RESET") {
                self.withheld += 1;
                return Ok(false);
            }
            let data = message["data"].as_object().ok_or(Code::Event)?;
            let reason = data
                .get("reason")
                .and_then(Value::as_str)
                .filter(|reason| REASONS.contains(reason))
                .ok_or(Code::Reason)?;
            let guest = data
                .get("guest")
                .and_then(Value::as_bool)
                .ok_or(Code::Guest)?;
            self.events
                .push(json!({"event":event,"reason":reason,"guest":guest}));
        } else if self.negotiated
            || object.len() != 2
            || message["id"] != "capabilities"
            || message["return"]
                .as_object()
                .is_none_or(|value| !value.is_empty())
        {
            return Err(Code::Negotiation);
        } else {
            self.negotiated = true;
        }
        Ok(false)
    }
    fn complete(&self) -> bool {
        self.failure.is_none()
            && self.connected
            && self.greeting
            && self.negotiated
            && self.eof
            && self.stream_close_attempted
            && self.stream_closed
            && self.parent_close_attempted
            && self.parent_closed
            && self.task_joined
            && !self.forced_stop
            && !self.cleanup_incomplete
    }
    fn diagnostics(&self) -> Value {
        json!({
            "connected":self.connected,
            "greetingReceived":self.greeting,
            "capabilitiesNegotiated":self.negotiated,
            "eofObserved":self.eof,
            "bytes":self.bytes,
            "messages":self.messages,
            "withheldEvents":self.withheld,
            "events":self.events,
            "failure":self.failure.map(Code::name),
            "streamCloseAttempted":self.stream_close_attempted,
            "streamClosed":self.stream_closed,
            "parentCloseAttempted":self.parent_close_attempted,
            "parentClosed":self.parent_closed,
            "taskJoined":self.task_joined,
            "forcedStop":self.forced_stop,
            "cleanupIncomplete":self.cleanup_incomplete,
            "preNegotiationEventsObservable":false,
            "absenceProvesNoEvent":false,
        })
    }
}

struct State {
    path: PathBuf,
    directory: PathBuf,
    parent: Option<File>,
    parent_stamp: Stamp,
    socket: Option<UnixStream>,
    facts: Facts,
    health: Arc<AtomicBool>,
    failure: FailureSlot,
}
impl State {
    fn open(
        path: &Path,
        end: Instant,
        health: &Arc<AtomicBool>,
        failure: &FailureSlot,
    ) -> Observed<Self> {
        live(Instant::now(), end)?;
        let directory = path.parent().ok_or(Code::Path)?;
        if !path.is_absolute()
            || path.file_name() != Some(OsStr::new("qmp.sock"))
            || path.as_os_str().as_bytes().len() > MAX_PATH
            || directory.canonicalize().map_err(|_| Code::Path)? != directory
        {
            return Err(Code::Path);
        }
        live(Instant::now(), end)?;
        let descriptor = rustix::fs::open(
            directory,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| Code::Io)?;
        let parent = File::from(descriptor);
        let admitted = (|| {
            live(Instant::now(), end)?;
            let metadata = parent.metadata().map_err(|_| Code::Io)?;
            live(Instant::now(), end)?;
            if !metadata.is_dir()
                || metadata.uid() != rustix::process::getuid().as_raw()
                || metadata.mode() & 0o077 != 0
            {
                return Err(Code::Parent);
            }
            let visible = fs::symlink_metadata(directory).map_err(|_| Code::Io)?;
            live(Instant::now(), end)?;
            let stamp = Stamp::from(&metadata);
            if stamp != Stamp::from(&visible) {
                return Err(Code::Namespace);
            }
            Ok(stamp)
        })();
        match admitted {
            Ok(parent_stamp) => Ok(Self {
                path: path.into(),
                directory: directory.into(),
                parent: Some(parent),
                parent_stamp,
                socket: None,
                facts: Facts::default(),
                health: health.clone(),
                failure: failure.clone(),
            }),
            Err(code) => {
                unhealthy(health, failure, code.error());
                let _ = nix::unistd::close(parent);
                Err(code)
            }
        }
    }
    fn fail(&mut self, code: Code) {
        self.facts.fail(code);
        unhealthy(&self.health, &self.failure, code.error());
    }
    fn namespace(&self, end: Instant) -> Observed<()> {
        live(Instant::now(), end)?;
        let original = self.parent.as_ref().ok_or(Code::Namespace)?;
        let held = original.metadata().map_err(|_| Code::Io)?;
        live(Instant::now(), end)?;
        let visible = fs::symlink_metadata(&self.directory).map_err(|_| Code::Io)?;
        live(Instant::now(), end)?;
        if self.parent_stamp == Stamp::from(&held) && self.parent_stamp == Stamp::from(&visible) {
            Ok(())
        } else {
            Err(Code::Namespace)
        }
    }
    fn close_stream(&mut self) {
        if let Some(socket) = self.socket.take() {
            self.facts.stream_close_attempted = true;
            match nix::unistd::close(socket) {
                Ok(()) => self.facts.stream_closed = true,
                Err(_) => self.fail(Code::Close),
            }
        }
    }
    fn close_parent(&mut self) {
        if let Some(parent) = self.parent.take() {
            self.facts.parent_close_attempted = true;
            match nix::unistd::close(parent) {
                Ok(()) => self.facts.parent_closed = true,
                Err(_) => self.fail(Code::Close),
            }
        }
    }
}
type Shared = Arc<Mutex<State>>;
fn locked(shared: &Shared) -> MutexGuard<'_, State> {
    match shared.lock() {
        Ok(state) => state,
        Err(poisoned) => {
            let mut state = poisoned.into_inner();
            state.fail(Code::Observer);
            state
        }
    }
}

async fn pause(end: Instant) {
    let until = clipped(Instant::now(), POLL, end);
    tokio::time::sleep_until(tokio::time::Instant::from_std(until)).await;
}
async fn connect(shared: &Shared, stop: &AtomicBool, until: Instant, end: Instant) -> Observed<()> {
    let expired = if until < end {
        Code::ConnectIncomplete
    } else {
        Code::Deadline
    };
    let phase = |result: Observed<()>| {
        result.map_err(|code| {
            if code == Code::Deadline {
                expired
            } else {
                code
            }
        })
    };
    loop {
        phase(live(Instant::now(), until))?;
        if stop.load(Ordering::Acquire) {
            return Err(Code::ConnectIncomplete);
        }
        let (path, socket_stamp) = {
            let state = locked(shared);
            phase(state.namespace(until))?;
            match fs::symlink_metadata(&state.path) {
                Ok(metadata) => {
                    phase(live(Instant::now(), until))?;
                    if !metadata.file_type().is_socket()
                        || metadata.uid() != rustix::process::getuid().as_raw()
                        || metadata.mode() & 0o077 != 0
                    {
                        return Err(Code::Socket);
                    }
                    (state.path.clone(), Some(Stamp::from(&metadata)))
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => (state.path.clone(), None),
                Err(_) => return Err(Code::Io),
            }
        };
        phase(live(Instant::now(), until))?;
        if let Some(socket_stamp) = socket_stamp {
            let attempt_end = clipped(Instant::now(), POLL, until);
            let connected = timeout_at(
                tokio::time::Instant::from_std(attempt_end),
                AsyncStream::connect(&path),
            )
            .await;
            let channel = match connected {
                Ok(Ok(channel)) => channel,
                Ok(Err(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    pause(until).await;
                    continue;
                }
                Ok(Err(_)) => return Err(Code::Io),
                Err(_) => return Err(Code::ConnectIncomplete),
            };
            locked(shared).facts.connected = true;
            // Conversion can consume its SDK-owned descriptor on error. That
            // path is negative and never contributes a checked-close fact.
            let channel = channel.into_std().map_err(|_| Code::Conversion)?;
            let mut state = locked(shared);
            state.socket = Some(channel);
            phase(live(Instant::now(), until))?;
            phase(state.namespace(until))?;
            let visible = fs::symlink_metadata(&state.path).map_err(|_| Code::Io)?;
            phase(live(Instant::now(), until))?;
            if Stamp::from(&visible) != socket_stamp {
                return Err(Code::Namespace);
            }
            state
                .socket
                .as_ref()
                .ok_or(Code::Socket)?
                .set_nonblocking(true)
                .map_err(|_| Code::Io)?;
            phase(live(Instant::now(), until))?;
            return Ok(());
        }
        pause(until).await;
    }
}
async fn capabilities(shared: &Shared, stop: &AtomicBool, end: Instant) -> Observed<()> {
    let mut offset = 0;
    while offset < CAPABILITIES.len() {
        live(Instant::now(), end)?;
        if stop.load(Ordering::Acquire) {
            return Err(Code::ObservationIncomplete);
        }
        let written = {
            let mut state = locked(shared);
            state.namespace(end)?;
            state
                .socket
                .as_mut()
                .ok_or(Code::Socket)?
                .write(&CAPABILITIES[offset..])
        };
        let blocked = match written {
            Ok(0) => return Err(Code::Io),
            Ok(count) => {
                offset += count;
                false
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => false,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => true,
            Err(_) => return Err(Code::Io),
        };
        live(Instant::now(), end)?;
        if blocked {
            pause(end).await;
        }
    }
    locked(shared).facts.greeting = true;
    Ok(())
}
async fn read(
    shared: &Shared,
    stop: &AtomicBool,
    closing: &AtomicBool,
    end: Instant,
) -> Observed<()> {
    let mut pending = Vec::new();
    loop {
        live(Instant::now(), end)?;
        if stop.load(Ordering::Acquire) {
            return Err(Code::ObservationIncomplete);
        }
        let mut buffer = [0; 4096];
        let observed = {
            let mut state = locked(shared);
            state.namespace(end)?;
            state.socket.as_mut().ok_or(Code::Socket)?.read(&mut buffer)
        };
        let count = match observed {
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                live(Instant::now(), end)?;
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                live(Instant::now(), end)?;
                pause(end).await;
                continue;
            }
            Err(_) => return Err(Code::Io),
        };
        {
            let mut state = locked(shared);
            state.facts.bytes = state
                .facts
                .bytes
                .checked_add(count)
                .ok_or(Code::ByteLimit)?;
            // Physical EOF/count remains visible even when the returned read
            // is late. Its original-end refusal precedes parser acceptance.
            if count == 0 {
                state.facts.eof = true;
            }
        }
        live(Instant::now(), end)?;
        if count == 0 {
            if !closing.load(Ordering::Acquire) {
                return Err(Code::ObservationIncomplete);
            }
            if pending.iter().any(|byte: &u8| !byte.is_ascii_whitespace())
                || !locked(shared).facts.negotiated
            {
                return Err(Code::EofIncomplete);
            }
            return Ok(());
        }
        if locked(shared).facts.bytes > MAX_BYTES {
            return Err(Code::ByteLimit);
        }
        pending.extend_from_slice(&buffer[..count]);
        while let Some(at) = pending.iter().position(|byte| *byte == b'\n') {
            if at > MAX_LINE {
                return Err(Code::LineLimit);
            }
            let send = locked(shared).facts.message(&pending[..at])?;
            pending.drain(..=at);
            live(Instant::now(), end)?;
            if send {
                capabilities(shared, stop, end).await?;
            }
        }
        if pending.len() > MAX_LINE {
            return Err(Code::LineLimit);
        }
    }
}
struct Health {
    health: Arc<AtomicBool>,
    failure: FailureSlot,
    handled: bool,
}
impl Health {
    fn new(health: Arc<AtomicBool>, failure: FailureSlot) -> Self {
        Self {
            health,
            failure,
            handled: false,
        }
    }
}
impl Drop for Health {
    fn drop(&mut self) {
        // A panic or unpolled cancellation has no ordinary returned Code.
        // Publish that fixed lifecycle refusal before Runner sees unhealthy.
        if !self.handled {
            record_error(&self.failure, Error::Close);
        }
        self.health.store(false, Ordering::Release);
    }
}
async fn observe(
    shared: Shared,
    stop: Arc<AtomicBool>,
    mut health: Health,
    closing: Arc<AtomicBool>,
    connect_end: Instant,
    end: Instant,
) {
    let result = async {
        connect(&shared, &stop, connect_end, end).await?;
        read(&shared, &stop, &closing, end).await
    }
    .await;
    if let Err(code) = result {
        unhealthy(&health.health, &health.failure, code.error());
    }
    let mut state = locked(&shared);
    if let Err(code) = result {
        state.fail(code);
    }
    state.close_stream();
    // Ordinary returned errors and expected closing EOF were handled above;
    // neither needs the guard's panic/cancellation fallback Error.
    health.handled = true;
}

/// The task borrows one slot-owned returned socket. No lock spans an await;
/// finish can still attempt that known close after a panic or forced stop.
pub(super) struct Observer {
    shared: Shared,
    stop: Arc<AtomicBool>,
    health: Arc<AtomicBool>,
    failure: FailureSlot,
    task: Option<JoinHandle<()>>,
    end: Instant,
    finished: bool,
}
impl Observer {
    pub(super) fn start(
        path: &Path,
        end: Instant,
        health: Arc<AtomicBool>,
        closing: Arc<AtomicBool>,
        failure: Arc<Mutex<Option<Error>>>,
    ) -> Result<Self> {
        let handle = tokio::runtime::Handle::try_current().map_err(|_| {
            unhealthy(&health, &failure, Code::Start.error());
            Code::Start.error()
        })?;
        let started = Instant::now();
        let mut state = State::open(path, end, &health, &failure).map_err(|code| {
            unhealthy(&health, &failure, code.error());
            code.error()
        })?;
        if let Err(code) = live(Instant::now(), end) {
            state.fail(code);
            state.close_parent();
            return Err(code.error());
        }
        let shared = Arc::new(Mutex::new(state));
        let stop = Arc::new(AtomicBool::new(false));
        let task = handle.spawn(observe(
            shared.clone(),
            stop.clone(),
            Health::new(health.clone(), failure.clone()),
            closing,
            clipped(started, CONNECT, end),
            end,
        ));
        Ok(Self {
            shared,
            stop,
            health,
            failure,
            task: Some(task),
            end,
            finished: false,
        })
    }
    pub(super) fn check(&self) -> Result<()> {
        let mut state = locked(&self.shared);
        if let Err(code) = live(Instant::now(), self.end) {
            state.fail(code);
        }
        if let Some(code) = state.facts.failure {
            unhealthy(&self.health, &self.failure, code.error());
        }
        state.facts.failure.map_or(Ok(()), |code| Err(code.error()))
    }
    pub(super) fn diagnostics(&self) -> Value {
        locked(&self.shared).facts.diagnostics()
    }
    async fn join_before(&mut self, end: Instant) -> bool {
        let Some(task) = self.task.as_mut() else {
            return locked(&self.shared).facts.task_joined;
        };
        let joined = if task.is_finished() {
            Some(task.await)
        } else if Instant::now() < end {
            timeout_at(tokio::time::Instant::from_std(end), task)
                .await
                .ok()
        } else {
            None
        };
        if let Some(joined) = joined {
            let _ = self.task.take();
            let mut state = locked(&self.shared);
            state.facts.task_joined = true;
            if joined.is_err() {
                state.fail(Code::Observer);
            }
            true
        } else {
            false
        }
    }
    pub(super) async fn finish(&mut self) -> Result<Value> {
        if !self.finished {
            if let Err(code) = live(Instant::now(), self.end) {
                locked(&self.shared).fail(code);
            }
            if !self
                .join_before(clipped(Instant::now(), CLOSE, self.end))
                .await
            {
                {
                    let mut state = locked(&self.shared);
                    state.facts.forced_stop = true;
                    state.fail(Code::ObservationIncomplete);
                }
                self.stop.store(true, Ordering::Release);
                if !self
                    .join_before(clipped(Instant::now(), CLOSE, self.end))
                    .await
                {
                    locked(&self.shared).fail(Code::CleanupIncomplete);
                    if let Some(task) = self.task.take() {
                        task.abort();
                        // A late/aborted observer remains negative, but no known
                        // task may outlive explicit close of its socket slot.
                        let _ = task.await;
                        locked(&self.shared).facts.task_joined = true;
                    }
                }
            }
            let mut state = locked(&self.shared);
            state.close_stream();
            state.close_parent();
            if let Err(code) = live(Instant::now(), self.end) {
                state.fail(code);
            }
            if !state.facts.complete() && state.facts.failure.is_none() {
                state.fail(Code::ObservationIncomplete);
            }
            self.finished = true;
        }
        self.check()?;
        let state = locked(&self.shared);
        if state.facts.complete() {
            Ok(state.facts.diagnostics())
        } else {
            Err(state
                .facts
                .failure
                .unwrap_or(Code::ObservationIncomplete)
                .error())
        }
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        if !self.finished || self.task.is_some() {
            unhealthy(&self.health, &self.failure, Error::Close);
            self.stop.store(true, Ordering::Release);
            if let Some(task) = self.task.take() {
                task.abort();
            }
            let mut state = locked(&self.shared);
            state.facts.forced_stop = true;
            state.fail(Code::CleanupIncomplete);
            state.close_stream();
            state.close_parent();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> Vec<u8> {
        serde_json::to_vec(&json!({"QMP":{"version":{"qemu":{
            "major":10,"minor":0,"micro":2},"package":"WITHHELD"},"capabilities":[]}}))
        .unwrap()
    }
    fn negotiated() -> Facts {
        let mut facts = Facts::default();
        assert_eq!(facts.message(&hello()), Ok(true));
        facts.greeting = true;
        assert_eq!(
            facts.message(br#"{"return":{},"id":"capabilities"}"#),
            Ok(false)
        );
        facts
    }
    #[test]
    fn only_closed_events_are_projected_and_unknown_data_is_withheld() {
        let mut facts = negotiated();
        assert_eq!(
            facts.message(br#"{"event":"OPAQUE_SECRET","data":{"secret":"WITHHELD"}}"#),
            Ok(false)
        );
        assert_eq!(
            facts.message(
                br#"{"event":"RESET","data":{"reason":"guest-reset","guest":true,
            "secret":"WITHHELD"},"timestamp":{"secret":"WITHHELD"}}"#
            ),
            Ok(false)
        );
        let value = facts.diagnostics();
        assert_eq!(value["withheldEvents"], 1);
        assert_eq!(
            value["events"],
            json!([{"event":"RESET","reason":"guest-reset","guest":true}])
        );
        let output = value.to_string();
        assert!(!output.contains("WITHHELD") && !output.contains("OPAQUE_SECRET"));
        assert_eq!(value["preNegotiationEventsObservable"], false);
        assert_eq!(value["absenceProvesNoEvent"], false);
    }
    #[test]
    fn greeting_and_capabilities_domains_remain_bounded() {
        for field in [json!(-1), json!(65536), json!(true), Value::Null] {
            let mut value: Value = serde_json::from_slice(&hello()).unwrap();
            value["QMP"]["version"]["qemu"]["major"] = field;
            assert_eq!(
                Facts::default().message(&serde_json::to_vec(&value).unwrap()),
                Err(Code::Greeting)
            );
        }
        let mut value: Value = serde_json::from_slice(&hello()).unwrap();
        value["QMP"]["capabilities"] = json!(vec![Value::Null; MAX_CAPABILITIES + 1]);
        assert_eq!(
            Facts::default().message(&serde_json::to_vec(&value).unwrap()),
            Err(Code::Greeting)
        );
        value["QMP"]["capabilities"] = json!(vec![Value::Null; MAX_CAPABILITIES]);
        assert_eq!(
            Facts::default().message(&serde_json::to_vec(&value).unwrap()),
            Ok(true)
        );
    }
    #[test]
    fn malformed_negotiation_and_lifecycle_fields_refuse_without_echo() {
        for raw in [
            br#"[]"#.as_slice(),
            br#"{"return":{},"id":"capabilities","id":"capabilities"}"#,
            br#"{"return":{},"id":"capabilities","extra":"WITHHELD"}"#,
            br#"{"return":{"secret":"WITHHELD"},"id":"capabilities"}"#,
        ] {
            let mut facts = Facts {
                greeting: true,
                ..Facts::default()
            };
            assert!(facts.message(raw).is_err());
        }
        for raw in [
            br#"{"event":"RESET","data":{"reason":"WITHHELD","guest":true}}"#.as_slice(),
            br#"{"event":"SHUTDOWN","data":{"reason":"guest-shutdown","guest":1}}"#,
        ] {
            let mut facts = negotiated();
            assert!(facts.message(raw).is_err());
            assert!(!facts.diagnostics().to_string().contains("WITHHELD"));
        }
    }
    #[test]
    fn unknown_messages_and_actual_negative_counts_remain_visible() {
        let mut facts = negotiated();
        assert_eq!(
            facts.message(br#"{"return":{},"id":"capabilities"}"#),
            Err(Code::Negotiation)
        );
        facts.messages = MAX_MESSAGES;
        assert_eq!(facts.message(br#"{}"#), Err(Code::MessageLimit));
        assert_eq!(facts.messages, MAX_MESSAGES + 1);
        facts.bytes = MAX_BYTES + 4096;
        facts.fail(Code::ByteLimit);
        assert_eq!(facts.diagnostics()["bytes"], MAX_BYTES + 4096);
        assert!(!facts.complete());
    }
    #[test]
    fn close_and_join_facts_cannot_be_replaced_by_protocol_flags() {
        let mut facts = negotiated();
        facts.connected = true;
        facts.eof = true;
        assert!(!facts.complete());
        facts.stream_close_attempted = true;
        facts.stream_closed = true;
        facts.parent_close_attempted = true;
        facts.parent_closed = true;
        facts.task_joined = true;
        assert!(facts.complete());
        facts.forced_stop = true;
        assert!(!facts.complete());
        facts.fail(Code::Io);
        facts.fail(Code::Close);
        assert_eq!(facts.failure, Some(Code::Io));
        facts.fail(Code::CleanupIncomplete);
        assert_eq!(facts.failure, Some(Code::Io));
        assert!(facts.cleanup_incomplete && !facts.complete());
    }
    #[test]
    fn termination_guard_only_lowers_shared_health() {
        let health = Arc::new(AtomicBool::new(true));
        let failure = Arc::new(Mutex::new(None));
        {
            let _guard = Health::new(health.clone(), failure.clone());
            assert!(health.load(Ordering::Acquire));
        }
        assert!(!health.load(Ordering::Acquire));
        assert_eq!(*failure.lock().unwrap(), Some(Error::Close));
        let mut facts = Facts::default();
        facts.fail(Code::Json);
        facts.fail(Code::CleanupIncomplete);
        assert_eq!(facts.failure, Some(Code::Json));
        assert_eq!(facts.diagnostics()["cleanupIncomplete"], true);
        assert!(!facts.complete());
    }

    #[test]
    fn an_unpolled_observer_future_still_lowers_health_when_cancelled() {
        let health = Arc::new(AtomicBool::new(true));
        let failure = Arc::new(Mutex::new(None));
        let guard = Health::new(health.clone(), failure.clone());
        let unpolled = async move {
            let _guard = guard;
        };
        assert!(health.load(Ordering::Acquire));
        drop(unpolled);
        assert!(!health.load(Ordering::Acquire));
        assert_eq!(*failure.lock().unwrap(), Some(Error::Close));
    }

    #[test]
    fn typed_first_error_is_published_before_health_and_survives_guard_cleanup() {
        for (code, expected) in [
            (Code::Deadline, Error::Deadline),
            (Code::Io, Error::Io),
            (Code::Close, Error::Close),
            (Code::Greeting, Error::Refused),
        ] {
            let health = Arc::new(AtomicBool::new(true));
            let failure = Arc::new(Mutex::new(None));
            unhealthy(&health, &failure, code.error());
            assert!(!health.load(Ordering::Acquire));
            assert_eq!(*failure.lock().unwrap(), Some(expected));
            drop(Health::new(health.clone(), failure.clone()));
            assert_eq!(*failure.lock().unwrap(), Some(expected));
        }
        let health = AtomicBool::new(true);
        let failure = Arc::new(Mutex::new(Some(Error::Child(7))));
        unhealthy(&health, &failure, Code::Io.error());
        assert!(!health.load(Ordering::Acquire));
        assert_eq!(*failure.lock().unwrap(), Some(Error::Child(7)));
    }

    #[test]
    fn handled_expected_eof_only_lowers_health_without_minting_error() {
        let health = Arc::new(AtomicBool::new(true));
        let failure = Arc::new(Mutex::new(None));
        let mut guard = Health::new(health.clone(), failure.clone());
        guard.handled = true;
        drop(guard);
        assert!(!health.load(Ordering::Acquire));
        assert_eq!(*failure.lock().unwrap(), None);
    }

    #[test]
    fn every_phase_and_post_io_fence_clips_the_same_original_end() {
        let start = Instant::now();
        let end = start + Duration::from_secs(30);
        assert_eq!(clipped(start + Duration::from_secs(29), CONNECT, end), end);
        assert_eq!(clipped(start + Duration::from_secs(29), CLOSE, end), end);
        assert_eq!(live(end - Duration::from_nanos(1), end), Ok(()));
        assert_eq!(live(end, end), Err(Code::Deadline));
        assert_eq!(
            live(end + Duration::from_nanos(1), end),
            Err(Code::Deadline)
        );
        // Actual late EOF is retained, not positive completion.
        let mut facts = Facts {
            eof: true,
            ..Facts::default()
        };
        facts.fail(live(end, end).unwrap_err());
        assert!(facts.eof && !facts.complete());
    }
}
