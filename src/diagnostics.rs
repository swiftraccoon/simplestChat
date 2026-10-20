#![forbid(unsafe_code)]

//! Opt-in, bounded, local operation diagnostics. No message data or free-form
//! fields enter this recorder. Producers never perform file I/O or wait for the
//! writer; full queues and limits are counted as missing coverage.
//! Timings are local monotonic elapsed time, not CPU time. Nested stages overlap.

use serde::Serialize;
use std::future::Future;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{
    AtomicBool, AtomicU64,
    Ordering::{AcqRel, Acquire, Relaxed, Release},
};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::Instrument;

const QUEUE_CAPACITY: usize = 256;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const SHUTDOWN_BUDGET: Duration = Duration::from_millis(200);
const CLOSED: u64 = 1 << 63;
const WRITER_PARK_BUDGET: Duration = Duration::from_millis(50);

/// Fixed operation vocabulary. No client-provided names become record fields.
#[derive(Clone, Copy, Debug)]
pub(crate) enum OperationKind {
    JoinRoom,
    LeaveRoom,
    Reconnect,
    RouterCapabilities,
    CreateSendTransport,
    CreateRecvTransport,
    ConnectTransport,
    Produce,
    Consume,
    ResumeConsumer,
    PauseConsumer,
    CloseConsumer,
    PauseProducer,
    ResumeProducer,
    CloseProducer,
    RestartIce,
    SetPreferredLayers,
    Chat,
    PrivateMessage,
    RoomAction,
    SocketWrite,
    ShutdownNotification,
}

impl OperationKind {
    /// The operation's name in records and metric labels: one vocabulary.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::JoinRoom => "join_room",
            Self::LeaveRoom => "leave_room",
            Self::Reconnect => "reconnect",
            Self::RouterCapabilities => "router_capabilities",
            Self::CreateSendTransport => "create_send_transport",
            Self::CreateRecvTransport => "create_recv_transport",
            Self::ConnectTransport => "connect_transport",
            Self::Produce => "produce",
            Self::Consume => "consume",
            Self::ResumeConsumer => "resume_consumer",
            Self::PauseConsumer => "pause_consumer",
            Self::CloseConsumer => "close_consumer",
            Self::PauseProducer => "pause_producer",
            Self::ResumeProducer => "resume_producer",
            Self::CloseProducer => "close_producer",
            Self::RestartIce => "restart_ice",
            Self::SetPreferredLayers => "set_preferred_layers",
            Self::Chat => "chat",
            Self::PrivateMessage => "private_message",
            Self::RoomAction => "room_action",
            Self::SocketWrite => "socket_write",
            Self::ShutdownNotification => "shutdown_notification",
        }
    }
}

impl Serialize for OperationKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.label())
    }
}

/// Fixed timing boundaries. A stage can occur multiple times in one operation.
/// Presence in this vocabulary does not imply that every path is instrumented.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Stage {
    RoomCreationLockWait,
    RoomLockWait,
    RoomPolicyLookup,
    RoomPasswordPermitWait,
    RoomMediaSetup,
    RoomMembershipCommit,
    SessionLockWait,
    MediaCreateTransport,
    MediaConnectTransport,
    MediaProduce,
    MediaConsume,
    MediaResume,
    SocketWrite,
    Dispatch,
}

/// `Error` means a returned error, not necessarily a service fault. `Completed`
/// means the future returned; its result was not classified. Dropping an active
/// future records `Cancelled`, including unwinding, rather than inventing success.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Outcome {
    Ok,
    Error,
    Completed,
    Cancelled,
    Timeout,
    Rejected,
}

/// Default and ceiling of `DIAGNOSTICS_MAX_RECORDS`.
pub(crate) const DEFAULT_MAX_RECORDS: u64 = 10_000;
pub(crate) const MAX_RECORDS_LIMIT: u64 = 100_000;
/// Default and ceiling of `DIAGNOSTICS_DURATION_SECS`.
pub(crate) const DEFAULT_DURATION_SECS: u64 = 300;
pub(crate) const MAX_DURATION_SECS: u64 = 3600;

#[derive(Clone, Copy)]
struct Limits {
    max_records: u64,
    duration: Duration,
}

impl Limits {
    fn validate(self) -> io::Result<Self> {
        if !(1..=MAX_RECORDS_LIMIT).contains(&self.max_records)
            || self.duration.is_zero()
            || self.duration > Duration::from_secs(MAX_DURATION_SECS)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid diagnostic limits",
            ));
        }
        Ok(self)
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_records: DEFAULT_MAX_RECORDS,
            duration: Duration::from_secs(DEFAULT_DURATION_SECS),
        }
    }
}

/// Shared recorder handle. Default is disabled: no thread, allocation, file,
/// clock read or connection-ID generation is required to construct it.
#[derive(Clone, Default)]
pub(crate) struct Diagnostics {
    inner: Option<Arc<Inner>>,
}

struct Inner {
    sender: mpsc::Sender<Record>,
    shared: Arc<Shared>,
    next_operation: AtomicU64,
}

struct Shared {
    started: Instant,
    limits: Limits,
    // High bit closes admission; low bits count short, synchronous producer
    // sections, never operation lifetimes or work that can await or perform I/O.
    producers: AtomicU64,
    writer_thread: OnceLock<std::thread::Thread>,
    reserved_records: AtomicU64,
    accepted: AtomicU64,
    written: AtomicU64,
    dropped: AtomicU64,
    expired: AtomicU64,
    started_records: AtomicU64,
    finalized_records: AtomicU64,
    unfinished_at_close: AtomicU64,
    write_failed: AtomicBool,
    finished: AtomicBool,
    finished_notify: tokio::sync::Notify,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.shared.close();
    }
}

/// Registration makes close wait on the writer thread for already-admitted
/// accounting/enqueues. It never excludes another producer or blocks a caller.
struct ProducerSection<'a>(&'a Shared);

impl Drop for ProducerSection<'_> {
    fn drop(&mut self) {
        if self.0.producers.fetch_sub(1, Release) == CLOSED + 1 {
            self.0.wake_writer();
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    schema_version: u8,
    kind: RecordKind,
    operation_id: u64,
    connection_id: Option<u64>,
    operation: OperationKind,
    stage: Option<Stage>,
    outcome: Outcome,
    started_us: u64,
    elapsed_us: u64,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum RecordKind {
    Operation,
    Stage,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Summary {
    schema_version: u8,
    kind: &'static str,
    accepted: u64,
    written: u64,
    dropped: u64,
    expired: u64,
    unfinished: u64,
    write_failed: bool,
}

impl Shared {
    fn enter(&self) -> Option<ProducerSection<'_>> {
        self.producers
            .try_update(Acquire, Relaxed, |state| {
                // Also prevent the count from overflowing into the closed bit.
                (state < CLOSED - 1).then(|| state + 1)
            })
            .ok()
            .map(|_| ProducerSection(self))
    }

    fn close(&self) {
        self.producers.fetch_or(CLOSED, AcqRel);
        self.wake_writer();
    }

    fn wake_writer(&self) {
        if let Some(thread) = self.writer_thread.get() {
            thread.unpark();
        }
    }

    fn reserve_record(&self) -> bool {
        self.reserved_records
            .try_update(Relaxed, Relaxed, |reserved| {
                (reserved < self.limits.max_records).then_some(reserved + 1)
            })
            .is_ok()
    }

    fn summary(&self) -> Summary {
        Summary {
            schema_version: 1,
            kind: "summary",
            accepted: self.accepted.load(Relaxed),
            written: self.written.load(Relaxed),
            dropped: self.dropped.load(Relaxed),
            expired: self.expired.load(Relaxed),
            unfinished: self.unfinished_at_close.load(Relaxed),
            write_failed: self.write_failed.load(Relaxed),
        }
    }
}

fn microseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros())
        .unwrap_or(MAX_SAFE_INTEGER)
        .min(MAX_SAFE_INTEGER)
}

fn next_id(counter: &AtomicU64) -> Option<u64> {
    counter
        .try_update(Relaxed, Relaxed, |value| {
            (value < MAX_SAFE_INTEGER).then_some(value + 1)
        })
        .ok()
        .map(|value| value + 1)
}

impl Diagnostics {
    /// Give in-process request tests the real recorder without process-global
    /// environment changes or a production configuration bypass.
    #[cfg(test)]
    pub(crate) fn with_test_writer(writer: impl Write + Send + 'static) -> io::Result<Self> {
        Self::with_writer(writer, Limits::default())
    }

    /// Enables a new private JSONL file only when `DIAGNOSTICS_PATH` is set.
    /// Existing files (including symlinks) are never overwritten. The path must
    /// be absolute; its parent must already exist.
    pub(crate) fn configure(settings: &crate::config::Diagnostics) -> io::Result<Self> {
        let Some(path) = &settings.path else {
            return Ok(Self::default());
        };
        let limits = Limits {
            max_records: settings.max_records,
            duration: settings.duration,
        }
        .validate()?;
        Self::open(path, limits)
    }

    fn open(path: &Path, limits: Limits) -> io::Result<Self> {
        limits.validate()?;
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "DIAGNOSTICS_PATH must be absolute",
            ));
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        Self::with_writer(file, limits)
    }

    fn with_writer(writer: impl Write + Send + 'static, limits: Limits) -> io::Result<Self> {
        let (diagnostics, receiver) = Self::channel(limits);
        let shared = diagnostics
            .inner
            .as_ref()
            .expect("enabled recorder")
            .shared
            .clone();
        // No join on a possibly blocked disk. Closing admission drains the
        // bounded channel; shutdown waits only SHUTDOWN_BUDGET for completion.
        std::thread::Builder::new()
            .name("diagnostics-writer".into())
            .spawn(move || {
                write_records(writer, receiver, &shared);
            })?;
        Ok(diagnostics)
    }

    fn channel(limits: Limits) -> (Self, mpsc::Receiver<Record>) {
        let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let shared = Arc::new(Shared {
            started: Instant::now(),
            limits,
            producers: AtomicU64::new(0),
            writer_thread: OnceLock::new(),
            reserved_records: AtomicU64::new(0),
            accepted: AtomicU64::new(0),
            written: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            expired: AtomicU64::new(0),
            started_records: AtomicU64::new(0),
            finalized_records: AtomicU64::new(0),
            unfinished_at_close: AtomicU64::new(0),
            write_failed: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            finished_notify: tokio::sync::Notify::new(),
        });
        (
            Self {
                inner: Some(Arc::new(Inner {
                    sender,
                    shared,
                    next_operation: AtomicU64::new(0),
                })),
            },
            receiver,
        )
    }

    /// Begins one short operation. Call [`Operation::finish`] with its actual
    /// result, or cancellation will be recorded when the guard is dropped.
    pub(crate) fn operation(&self, kind: OperationKind, connection_id: Option<u64>) -> Operation {
        let active = self.inner.as_ref().and_then(|inner| {
            let id = next_id(&inner.next_operation)?;
            if !self.begin_record() {
                return None;
            }
            Some(Active {
                context: Context {
                    diagnostics: self.clone(),
                    id,
                    kind,
                    connection_id,
                },
                started: Instant::now(),
                stage: None,
            })
        });
        let span = match &active {
            Some(active) => tracing::debug_span!(target: "simplestchat::diagnostics", "operation",
                operation = ?kind, operation_id = active.context.id,
                connection_id = connection_id, outcome = tracing::field::Empty),
            None => tracing::Span::none(),
        };
        Operation { active, span }
    }

    fn begin_record(&self) -> bool {
        let Some(inner) = &self.inner else {
            return false;
        };
        let Some(_section) = inner.shared.enter() else {
            return false;
        };
        if inner.shared.started.elapsed() >= inner.shared.limits.duration {
            inner.shared.expired.fetch_add(1, Relaxed);
            return false;
        }
        if inner.shared.accepted.load(Relaxed) >= inner.shared.limits.max_records {
            inner.shared.dropped.fetch_add(1, Relaxed);
            return false;
        }
        inner.shared.started_records.fetch_add(1, Relaxed);
        true
    }

    fn record(&self, record: Record) {
        let Some(inner) = &self.inner else {
            return;
        };
        let shared = &inner.shared;
        let Some(_section) = shared.enter() else {
            // Timers crossing the close boundary remain unfinished in the
            // terminal snapshot, even if they finish while the writer drains.
            return;
        };
        if shared.started.elapsed() >= shared.limits.duration {
            shared.expired.fetch_add(1, Relaxed);
        } else if !shared.reserve_record() {
            shared.dropped.fetch_add(1, Relaxed);
        } else if inner.sender.try_send(record).is_ok() {
            shared.accepted.fetch_add(1, Relaxed);
            shared.wake_writer();
        } else {
            shared.reserved_records.fetch_sub(1, Relaxed);
            shared.dropped.fetch_add(1, Relaxed);
        }
        shared.finalized_records.fetch_add(1, Relaxed);
    }

    /// Stops accepting records and waits at most 200ms for local output. A false
    /// result means the file may be incomplete. It must never change chat's
    /// success/failure result or extend an existing drain indefinitely. True
    /// acknowledges writer completion, not coverage; inspect the summary for
    /// dropped, expired or unfinished records.
    pub(crate) async fn shutdown(&self) -> bool {
        let Some(inner) = &self.inner else {
            return true;
        };
        let close = async {
            inner.shared.close();
            loop {
                let notified = inner.shared.finished_notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if inner.shared.finished.load(Acquire) {
                    break;
                }
                notified.await;
            }
            !inner.shared.write_failed.load(Relaxed)
        };
        tokio::time::timeout(SHUTDOWN_BUDGET, close)
            .await
            .unwrap_or(false)
    }
}

fn write_line(
    writer: &mut impl Write,
    buffer: &mut Vec<u8>,
    value: &impl Serialize,
) -> io::Result<()> {
    buffer.clear();
    serde_json::to_writer(&mut *buffer, value)?;
    buffer.push(b'\n');
    if buffer.len() > 1024 {
        return Err(io::Error::other(
            "Diagnostic record exceeds the schema size limit",
        ));
    }
    writer.write_all(buffer)
}

fn write_records(mut writer: impl Write, mut receiver: mpsc::Receiver<Record>, shared: &Shared) {
    let _ = shared.writer_thread.set(std::thread::current());
    let mut buffer = Vec::with_capacity(1024);
    let mut closed = false;
    loop {
        if !closed && shared.producers.load(Acquire) == CLOSED {
            // No section can start now, and the release/acquire gate makes all
            // admitted counter updates visible. Drain only after this boundary;
            // an empty queue alone would miss a concurrent enqueue/accounting.
            receiver.close();
            shared.unfinished_at_close.store(
                shared
                    .started_records
                    .load(Relaxed)
                    .saturating_sub(shared.finalized_records.load(Relaxed)),
                Relaxed,
            );
            closed = true;
        }
        match receiver.try_recv() {
            Ok(record) => {
                if !shared.write_failed.load(Relaxed) {
                    if write_line(&mut writer, &mut buffer, &record).is_ok() {
                        shared.written.fetch_add(1, Relaxed);
                    } else {
                        shared.write_failed.store(true, Relaxed);
                        shared.close();
                    }
                }
            }
            Err(mpsc::error::TryRecvError::Empty) if !closed => {
                // unpark retains a token if an enqueue races this park. The
                // timeout also bounds recovery from a spurious/missed wake.
                std::thread::park_timeout(WRITER_PARK_BUDGET);
            }
            Err(mpsc::error::TryRecvError::Disconnected) if !closed => shared.close(),
            Err(_) => break,
        }
    }
    if writer.flush().is_err() {
        shared.write_failed.store(true, Relaxed);
    }
    if write_line(&mut writer, &mut buffer, &shared.summary())
        .and_then(|()| writer.flush())
        .is_err()
    {
        shared.write_failed.store(true, Relaxed);
    }
    shared.finished.store(true, Release);
    shared.finished_notify.notify_waiters();
}

#[derive(Clone)]
struct Context {
    diagnostics: Diagnostics,
    id: u64,
    kind: OperationKind,
    connection_id: Option<u64>,
}

tokio::task_local! {
    static CURRENT_OPERATION: Context;
}

struct Active {
    context: Context,
    started: Instant,
    stage: Option<Stage>,
}

impl Active {
    fn finish(self, outcome: Outcome) {
        let shared = &self
            .context
            .diagnostics
            .inner
            .as_ref()
            .expect("active recorder")
            .shared;
        let record = Record {
            schema_version: 1,
            kind: if self.stage.is_some() {
                RecordKind::Stage
            } else {
                RecordKind::Operation
            },
            operation_id: self.context.id,
            connection_id: self.context.connection_id,
            operation: self.context.kind,
            stage: self.stage,
            outcome,
            started_us: microseconds(self.started.duration_since(shared.started)),
            elapsed_us: microseconds(self.started.elapsed()),
        };
        self.context.diagnostics.record(record);
    }
}

/// RAII operation timer. Context is scoped to polls, not an entered thread-local
/// tracing span held across `await`. Detached tasks do not inherit it implicitly.
pub(crate) struct Operation {
    active: Option<Active>,
    span: tracing::Span,
}

impl Operation {
    /// Runs a future under this operation's timing context. Spawned tasks must
    /// establish their own explicit scope; no application data is captured.
    pub(crate) async fn scope<T>(&self, future: impl Future<Output = T>) -> T {
        match &self.active {
            Some(active) => {
                CURRENT_OPERATION
                    .scope(active.context.clone(), future.instrument(self.span.clone()))
                    .await
            }
            None => future.await,
        }
    }

    /// Completes a record with a known outcome; consuming the timer prevents
    /// duplicate completion on its later drop.
    pub(crate) fn finish(mut self, outcome: Outcome) {
        self.span.record("outcome", tracing::field::debug(outcome));
        if let Some(active) = self.active.take() {
            active.finish(outcome);
        }
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            self.span
                .record("outcome", tracing::field::debug(Outcome::Cancelled));
            active.finish(Outcome::Cancelled);
        }
    }
}

struct StageGuard(Option<Active>);

impl StageGuard {
    fn new(stage: Stage) -> Self {
        Self(
            CURRENT_OPERATION
                .try_with(|context| {
                    context.diagnostics.begin_record().then(|| Active {
                        context: context.clone(),
                        started: Instant::now(),
                        stage: Some(stage),
                    })
                })
                .ok()
                .flatten(),
        )
    }

    fn finish(mut self, outcome: Outcome) {
        if let Some(active) = self.0.take() {
            active.finish(outcome);
        }
    }
}

impl Drop for StageGuard {
    fn drop(&mut self) {
        if let Some(active) = self.0.take() {
            active.finish(Outcome::Cancelled);
        }
    }
}

/// Measures elapsed time until a future returns, without interpreting its value.
/// In particular, returning `Result::Err` is still `Completed` with this helper.
pub(crate) async fn measure<T>(stage: Stage, future: impl Future<Output = T>) -> T {
    let guard = StageGuard::new(stage);
    let result = future.await;
    guard.finish(Outcome::Completed);
    result
}

/// Measures a single Result-returning boundary. Nested Results and values that
/// encode failure need explicit handling; this checks only the outer Result.
pub(crate) async fn measure_result<T, E>(
    stage: Stage,
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, E> {
    let guard = StageGuard::new(stage);
    let result = future.await;
    guard.finish(if result.is_ok() {
        Outcome::Ok
    } else {
        Outcome::Error
    });
    result
}

#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;
