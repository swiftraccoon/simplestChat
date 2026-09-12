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
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
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

/// Fixed timing boundaries. A stage can occur multiple times in one operation.
/// Presence in this vocabulary does not imply that every path is instrumented.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    RoomCreationLockWait,
    RoomLockWait,
    RoomPolicyLookup,
    RoomPasswordPermitWait,
    RoomPasswordDispatch,
    RoomPasswordWork,
    RoomMediaSetup,
    RoomMembershipCommit,
    SessionLockWait,
    MediaCreateTransport,
    MediaConnectTransport,
    MediaProduce,
    MediaConsume,
    MediaResume,
    OutboundQueueWait,
    SocketWrite,
    Dispatch,
}

/// `Error` means a returned error, not necessarily a service fault. `Completed`
/// means the future returned; its result was not classified. Dropping an active
/// future records `Cancelled`, including unwinding, rather than inventing success.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Error,
    Completed,
    Cancelled,
    Timeout,
    Rejected,
}

#[derive(Clone, Copy)]
struct Limits {
    max_records: u64,
    duration: Duration,
}

impl Limits {
    fn validate(self) -> io::Result<Self> {
        if !(1..=100_000).contains(&self.max_records)
            || self.duration.is_zero()
            || self.duration > Duration::from_secs(3600)
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
            max_records: 10_000,
            duration: Duration::from_secs(300),
        }
    }
}

/// Shared recorder handle. Default is disabled: no thread, allocation, file,
/// clock read or connection-ID generation is required to construct it.
#[derive(Clone, Default)]
pub struct Diagnostics {
    inner: Option<Arc<Inner>>,
}

struct Inner {
    sender: mpsc::Sender<Record>,
    shared: Arc<Shared>,
    next_operation: AtomicU64,
    next_connection: AtomicU64,
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
            .fetch_update(Acquire, Relaxed, |state| {
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
            .fetch_update(Relaxed, Relaxed, |reserved| {
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
        .fetch_update(Relaxed, Relaxed, |value| {
            (value < MAX_SAFE_INTEGER).then_some(value + 1)
        })
        .ok()
        .map(|value| value + 1)
}

fn env_number(name: &'static str, default: u64) -> io::Result<u64> {
    match std::env::var(name) {
        Ok(value) => value
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, name)),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(_) => Err(io::Error::new(io::ErrorKind::InvalidInput, name)),
    }
}

impl Diagnostics {
    /// Enables a new private JSONL file only when `DIAGNOSTICS_PATH` is set.
    /// Existing files (including symlinks) are never overwritten. The path must
    /// be absolute; its parent must already exist. Numeric limits fail closed.
    pub fn from_env() -> io::Result<Self> {
        let Some(path) = std::env::var_os("DIAGNOSTICS_PATH") else {
            return Ok(Self::default());
        };
        let defaults = Limits::default();
        let limits = Limits {
            max_records: env_number("DIAGNOSTICS_MAX_RECORDS", defaults.max_records)?,
            duration: Duration::from_secs(env_number(
                "DIAGNOSTICS_DURATION_SECS",
                defaults.duration.as_secs(),
            )?),
        }
        .validate()?;
        Self::open(Path::new(&path), limits)
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
                    next_connection: AtomicU64::new(0),
                })),
            },
            receiver,
        )
    }

    /// Process-local reference with no relationship to account, room, reconnect
    /// token or media identity. References restart in each diagnostic file.
    pub fn connection_id(&self) -> Option<u64> {
        self.inner
            .as_ref()
            .and_then(|inner| next_id(&inner.next_connection))
    }

    /// Begins one short operation. Call [`Operation::finish`] with its actual
    /// result, or cancellation will be recorded when the guard is dropped.
    pub fn operation(&self, kind: OperationKind, connection_id: Option<u64>) -> Operation {
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
            Some(active) => tracing::debug_span!(target: "simplestChat::diagnostics", "operation",
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
    pub async fn shutdown(&self) -> bool {
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
pub struct Operation {
    active: Option<Active>,
    span: tracing::Span,
}

impl Operation {
    /// Runs a future under this operation's timing context. Spawned tasks must
    /// establish their own explicit scope; no application data is captured.
    pub async fn scope<T>(&self, future: impl Future<Output = T>) -> T {
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
    pub fn finish(mut self, outcome: Outcome) {
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
pub async fn measure<T>(stage: Stage, future: impl Future<Output = T>) -> T {
    let guard = StageGuard::new(stage);
    let result = future.await;
    guard.finish(Outcome::Completed);
    result
}

/// Measures a single Result-returning boundary. Nested Results and values that
/// encode failure need explicit handling; this checks only the outer Result.
pub async fn measure_result<T, E>(
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
mod tests {
    use super::*;
    use std::sync::{Barrier, Mutex, mpsc as sync_mpsc};

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Write for Buffer {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Buffer {
        fn records(&self) -> Vec<serde_json::Value> {
            let bytes = self.0.lock().unwrap();
            std::str::from_utf8(&bytes)
                .unwrap()
                .lines()
                .map(|line| {
                    assert!(line.len() <= 1024);
                    serde_json::from_str(line).unwrap()
                })
                .collect()
        }
    }

    #[tokio::test]
    async fn disabled_records_nothing_and_preserves_future_results() {
        let diagnostics = Diagnostics::default();
        assert!(diagnostics.inner.is_none());
        assert_eq!(diagnostics.connection_id(), None);
        let operation = diagnostics.operation(OperationKind::Chat, None);
        assert_eq!(
            operation
                .scope(measure(Stage::Dispatch, async { 42 }))
                .await,
            42
        );
        operation.finish(Outcome::Ok);
        assert!(diagnostics.shutdown().await);
    }

    #[tokio::test]
    async fn scopes_correlate_without_capturing_values_and_preserve_errors() {
        let output = Buffer::default();
        let diagnostics = Diagnostics::with_writer(output.clone(), Limits::default()).unwrap();
        let id = diagnostics.connection_id();
        let operation = diagnostics.operation(OperationKind::JoinRoom, id);
        let result = operation
            .scope(measure_result(Stage::RoomPolicyLookup, async {
                Err::<(), _>("PRIVATE_MESSAGE_TOKEN_SDP_NAME")
            }))
            .await;
        assert!(result.is_err());
        operation.finish(Outcome::Error);
        assert!(diagnostics.shutdown().await);
        let records = output.records();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0]["stage"], "room_policy_lookup");
        assert_eq!(records[0]["outcome"], "error");
        assert_eq!(records[0]["operationId"], records[1]["operationId"]);
        assert_eq!(records[1]["connectionId"], id.unwrap());
        assert!(records[1]["stage"].is_null());
        assert_eq!(records[2]["written"], 2);
        assert!(!serde_json::to_string(&records).unwrap().contains("PRIVATE"));
    }

    #[tokio::test]
    async fn dropped_pending_scope_records_stage_and_operation_cancellation() {
        let output = Buffer::default();
        let diagnostics = Diagnostics::with_writer(output.clone(), Limits::default()).unwrap();
        let operation = diagnostics.operation(OperationKind::Consume, None);
        {
            let mut future = Box::pin(
                operation.scope(measure(Stage::MediaConsume, std::future::pending::<()>())),
            );
            assert!(futures_util::poll!(&mut future).is_pending());
        }
        drop(operation);
        assert!(diagnostics.shutdown().await);
        let records = output.records();
        assert_eq!(records[0]["outcome"], "cancelled");
        assert_eq!(records[1]["outcome"], "cancelled");
    }

    #[tokio::test]
    async fn spawned_tasks_do_not_inherit_an_unrelated_operation() {
        let output = Buffer::default();
        let diagnostics = Diagnostics::with_writer(output.clone(), Limits::default()).unwrap();
        let operation = diagnostics.operation(OperationKind::JoinRoom, None);
        operation
            .scope(async {
                tokio::spawn(measure(Stage::Dispatch, async {}))
                    .await
                    .unwrap();
            })
            .await;
        operation.finish(Outcome::Ok);
        assert!(diagnostics.shutdown().await);
        assert_eq!(output.records().len(), 2);
    }

    #[test]
    fn record_limit_and_full_queue_are_nonblocking_and_counted() {
        let limits = Limits {
            max_records: 2,
            ..Limits::default()
        };
        let (diagnostics, mut receiver) = Diagnostics::channel(limits);
        for _ in 0..3 {
            diagnostics
                .operation(OperationKind::Chat, None)
                .finish(Outcome::Ok);
        }
        let inner = diagnostics.inner.as_ref().unwrap();
        assert_eq!(inner.shared.accepted.load(Relaxed), 2);
        assert_eq!(inner.shared.dropped.load(Relaxed), 1);
        assert_eq!(std::iter::from_fn(|| receiver.try_recv().ok()).count(), 2);

        let (diagnostics, _receiver) = Diagnostics::channel(Limits::default());
        for _ in 0..QUEUE_CAPACITY + 1 {
            diagnostics
                .operation(OperationKind::Chat, None)
                .finish(Outcome::Ok);
        }
        let shared = &diagnostics.inner.as_ref().unwrap().shared;
        assert_eq!(shared.accepted.load(Relaxed), QUEUE_CAPACITY as u64);
        assert_eq!(shared.dropped.load(Relaxed), 1);
    }

    #[test]
    fn concurrent_producers_below_queue_capacity_do_not_lose_records() {
        const PRODUCERS: usize = 8;
        const PER_PRODUCER: usize = 16;
        let (diagnostics, mut receiver) = Diagnostics::channel(Limits::default());
        let shared = &diagnostics.inner.as_ref().unwrap().shared;
        // A registered producer does not prevent other producers from entering.
        let held = shared.enter().unwrap();
        let barrier = Barrier::new(PRODUCERS);
        std::thread::scope(|threads| {
            for _ in 0..PRODUCERS {
                threads.spawn(|| {
                    barrier.wait();
                    for _ in 0..PER_PRODUCER {
                        diagnostics
                            .operation(OperationKind::Chat, None)
                            .finish(Outcome::Ok);
                    }
                });
            }
        });
        drop(held);
        let records: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
        let ids: std::collections::HashSet<_> =
            records.iter().map(|record| record.operation_id).collect();
        let expected = (PRODUCERS * PER_PRODUCER) as u64;
        assert_eq!(records.len() as u64, expected);
        assert_eq!(ids.len() as u64, expected);
        assert_eq!(shared.accepted.load(Relaxed), expected);
        assert_eq!(shared.reserved_records.load(Relaxed), expected);
        assert_eq!(shared.started_records.load(Relaxed), expected);
        assert_eq!(shared.finalized_records.load(Relaxed), expected);
        assert_eq!(shared.dropped.load(Relaxed), 0);
        assert_eq!(shared.producers.load(Relaxed), 0);
    }

    #[test]
    fn concurrent_record_reservations_enforce_the_limit() {
        let limits = Limits {
            max_records: 64,
            ..Limits::default()
        };
        let (diagnostics, mut receiver) = Diagnostics::channel(limits);
        let barrier = Barrier::new(8);
        std::thread::scope(|threads| {
            for _ in 0..8 {
                threads.spawn(|| {
                    barrier.wait();
                    for _ in 0..16 {
                        diagnostics
                            .operation(OperationKind::Chat, None)
                            .finish(Outcome::Ok);
                    }
                });
            }
        });
        let shared = &diagnostics.inner.as_ref().unwrap().shared;
        assert_eq!(std::iter::from_fn(|| receiver.try_recv().ok()).count(), 64);
        assert_eq!(shared.accepted.load(Relaxed), 64);
        assert_eq!(shared.reserved_records.load(Relaxed), 64);
        assert_eq!(shared.dropped.load(Relaxed), 64);
        assert_eq!(
            shared.started_records.load(Relaxed),
            shared.finalized_records.load(Relaxed)
        );
    }

    #[test]
    fn failed_enqueue_refunds_its_record_reservation() {
        let limits = Limits {
            max_records: QUEUE_CAPACITY as u64 + 1,
            ..Limits::default()
        };
        let (diagnostics, mut receiver) = Diagnostics::channel(limits);
        for _ in 0..QUEUE_CAPACITY + 1 {
            diagnostics
                .operation(OperationKind::Chat, None)
                .finish(Outcome::Ok);
        }
        let shared = &diagnostics.inner.as_ref().unwrap().shared;
        assert_eq!(shared.dropped.load(Relaxed), 1);
        assert_eq!(shared.reserved_records.load(Relaxed), QUEUE_CAPACITY as u64);
        receiver.try_recv().unwrap();
        diagnostics
            .operation(OperationKind::Chat, None)
            .finish(Outcome::Ok);
        assert_eq!(shared.accepted.load(Relaxed), QUEUE_CAPACITY as u64 + 1);
        assert_eq!(
            shared.reserved_records.load(Relaxed),
            QUEUE_CAPACITY as u64 + 1
        );
        assert_eq!(shared.dropped.load(Relaxed), 1);
    }

    #[test]
    fn duration_limit_and_disconnected_writer_are_observable() {
        let (diagnostics, mut receiver) = Diagnostics::channel(Limits {
            duration: Duration::ZERO,
            ..Limits::default()
        });
        diagnostics
            .operation(OperationKind::Chat, None)
            .finish(Outcome::Ok);
        assert_eq!(
            diagnostics
                .inner
                .as_ref()
                .unwrap()
                .shared
                .expired
                .load(Relaxed),
            1
        );
        assert!(receiver.try_recv().is_err());
        let (diagnostics, receiver) = Diagnostics::channel(Limits::default());
        drop(receiver);
        diagnostics
            .operation(OperationKind::Chat, None)
            .finish(Outcome::Ok);
        assert_eq!(
            diagnostics
                .inner
                .as_ref()
                .unwrap()
                .shared
                .dropped
                .load(Relaxed),
            1
        );
    }

    #[tokio::test]
    async fn output_failure_never_changes_the_operation_result() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("failed fixture"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::other("failed fixture"))
            }
        }
        let diagnostics = Diagnostics::with_writer(Broken, Limits::default()).unwrap();
        let operation = diagnostics.operation(OperationKind::Chat, None);
        assert_eq!(operation.scope(async { 42 }).await, 42);
        operation.finish(Outcome::Ok);
        assert!(!diagnostics.shutdown().await);
        assert_eq!(
            diagnostics
                .inner
                .as_ref()
                .unwrap()
                .shared
                .written
                .load(Relaxed),
            0
        );
    }

    #[tokio::test]
    async fn blocked_writer_cannot_extend_shutdown_past_its_budget() {
        struct Blocked {
            started: sync_mpsc::Sender<()>,
            release: sync_mpsc::Receiver<()>,
        }
        impl Write for Blocked {
            fn write(&mut self, data: &[u8]) -> io::Result<usize> {
                self.started.send(()).ok();
                self.release.recv_timeout(Duration::from_secs(2)).ok();
                Ok(data.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (started_tx, started_rx) = sync_mpsc::channel();
        let (release_tx, release_rx) = sync_mpsc::channel();
        let diagnostics = Diagnostics::with_writer(
            Blocked {
                started: started_tx,
                release: release_rx,
            },
            Limits::default(),
        )
        .unwrap();
        diagnostics
            .operation(OperationKind::Chat, None)
            .finish(Outcome::Ok);
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        for _ in 0..QUEUE_CAPACITY + 1 {
            diagnostics
                .operation(OperationKind::Chat, None)
                .finish(Outcome::Ok);
        }
        let shared = &diagnostics.inner.as_ref().unwrap().shared;
        assert_eq!(shared.accepted.load(Relaxed), QUEUE_CAPACITY as u64 + 1);
        assert_eq!(shared.dropped.load(Relaxed), 1);
        assert!(!diagnostics.shutdown().await);
        release_tx.send(()).unwrap();
        drop(release_tx);
        assert!(diagnostics.shutdown().await);
    }

    #[tokio::test]
    async fn shutdown_reports_unfinished_timers_and_rejects_late_records() {
        let output = Buffer::default();
        let diagnostics = Diagnostics::with_writer(output.clone(), Limits::default()).unwrap();
        let operation = diagnostics.operation(OperationKind::Consume, None);
        let mut future =
            Box::pin(operation.scope(measure(Stage::MediaConsume, std::future::pending::<()>())));
        assert!(futures_util::poll!(&mut future).is_pending());
        assert!(diagnostics.shutdown().await);
        let records = output.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["unfinished"], 2);
        drop(future);
        drop(operation);
        diagnostics
            .operation(OperationKind::Chat, None)
            .finish(Outcome::Ok);
        assert!(diagnostics.shutdown().await);
        assert_eq!(output.records(), records);
    }

    #[tokio::test]
    async fn close_racing_producers_accounts_for_every_admitted_timer() {
        let output = Buffer::default();
        let diagnostics = Diagnostics::with_writer(output.clone(), Limits::default()).unwrap();
        let barrier = Arc::new(Barrier::new(9));
        let admitted = Arc::new(AtomicU64::new(0));
        let mut threads = Vec::new();
        for _ in 0..8 {
            let diagnostics = diagnostics.clone();
            let barrier = barrier.clone();
            let admitted = admitted.clone();
            threads.push(std::thread::spawn(move || {
                let first = diagnostics.operation(OperationKind::Chat, None);
                admitted.fetch_add(u64::from(first.active.is_some()), Relaxed);
                barrier.wait();
                first.finish(Outcome::Ok);
                for _ in 0..15 {
                    let operation = diagnostics.operation(OperationKind::Chat, None);
                    admitted.fetch_add(u64::from(operation.active.is_some()), Relaxed);
                    operation.finish(Outcome::Ok);
                }
            }));
        }
        barrier.wait();
        let completed = diagnostics.shutdown().await;
        for thread in threads {
            thread.join().unwrap();
        }
        assert!(completed);
        let records = output.records();
        let summary = records.last().unwrap();
        let accepted = summary["accepted"].as_u64().unwrap();
        let unfinished = summary["unfinished"].as_u64().unwrap();
        assert_eq!(accepted + unfinished, admitted.load(Relaxed));
        assert_eq!(accepted as usize, records.len() - 1);
        assert_eq!(summary["written"], accepted);
        assert_eq!(summary["dropped"], 0);
        assert_eq!(summary["expired"], 0);
        assert!(
            diagnostics
                .operation(OperationKind::Chat, None)
                .active
                .is_none()
        );
        assert_eq!(output.records(), records);
    }

    #[tokio::test]
    async fn writer_waits_for_registered_sections_before_its_terminal_snapshot() {
        let output = Buffer::default();
        let diagnostics = Diagnostics::with_writer(output.clone(), Limits::default()).unwrap();
        let shared = &diagnostics.inner.as_ref().unwrap().shared;
        let section = shared.enter().unwrap();
        let mut shutdown = Box::pin(diagnostics.shutdown());
        assert!(futures_util::poll!(&mut shutdown).is_pending());
        assert_eq!(shared.producers.load(Relaxed), CLOSED + 1);
        assert!(shared.enter().is_none());
        assert!(!shared.finished.load(Acquire));
        // Model an already-admitted timer start finishing its short section.
        shared.started_records.fetch_add(1, Relaxed);
        drop(section);
        assert!(shutdown.await);
        let records = output.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["unfinished"], 1);
    }

    #[tokio::test]
    async fn concurrent_shutdown_callers_observe_the_same_writer_completion() {
        let diagnostics = Diagnostics::with_writer(Buffer::default(), Limits::default()).unwrap();
        diagnostics
            .operation(OperationKind::Chat, None)
            .finish(Outcome::Ok);
        let (first, second) = tokio::join!(diagnostics.shutdown(), diagnostics.shutdown());
        assert!(first && second);
    }

    #[tokio::test]
    async fn output_is_private_create_new_and_limits_are_validated() {
        let directory =
            std::env::temp_dir().join(format!("simplestchat-diagnostics-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("events.jsonl");
        let diagnostics = Diagnostics::open(&path, Limits::default()).unwrap();
        assert!(Diagnostics::open(&path, Limits::default()).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let link = directory.join("link");
            symlink(&path, &link).unwrap();
            assert!(Diagnostics::open(&link, Limits::default()).is_err());
            std::fs::remove_file(link).unwrap();
        }
        assert!(diagnostics.shutdown().await);
        assert!(Diagnostics::open(Path::new("relative"), Limits::default()).is_err());
        assert!(
            Limits {
                max_records: 100_001,
                ..Limits::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Limits {
                max_records: 0,
                ..Limits::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Limits {
                duration: Duration::ZERO,
                ..Limits::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Limits {
                duration: Duration::from_secs(3601),
                ..Limits::default()
            }
            .validate()
            .is_err()
        );
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
