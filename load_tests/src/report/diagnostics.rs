//! Read the stopped server's private JSONL recorder using the reference policy.
//!
//! Only fixed vocabulary, aggregate durations and terminal counts are exported.
//! The file is opened without following its final symlink or blocking on special
//! files; its actual descriptor must name a private regular file. Bounds apply
//! both before and during reading, including files that grow after opening.

use nix::fcntl::{OFlag, open};
use nix::sys::stat::Mode;
use serde::Serialize;
use serde_json::value::RawValue;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// Maximum combined operation and stage records, excluding the terminal summary.
pub const MAX_RECORDS: usize = 100_000;
/// Maximum UTF-8 JSON bytes before each newline.
pub const LINE_BYTES: usize = 1024;
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
const OPERATIONS: &[&str] = &[
    "join_room",
    "leave_room",
    "reconnect",
    "router_capabilities",
    "create_send_transport",
    "create_recv_transport",
    "connect_transport",
    "produce",
    "consume",
    "resume_consumer",
    "pause_consumer",
    "close_consumer",
    "pause_producer",
    "resume_producer",
    "close_producer",
    "restart_ice",
    "set_preferred_layers",
    "chat",
    "private_message",
    "room_action",
    "socket_write",
    "shutdown_notification",
];
const STAGES: &[&str] = &[
    "room_creation_lock_wait",
    "room_lock_wait",
    "room_policy_lookup",
    "room_password_permit_wait",
    "room_password_dispatch",
    "room_password_work",
    "room_media_setup",
    "room_membership_commit",
    "session_lock_wait",
    "media_create_transport",
    "media_connect_transport",
    "media_produce",
    "media_consume",
    "media_resume",
    "outbound_queue_wait",
    "socket_write",
    "dispatch",
];
const OUTCOMES: &[&str] = &[
    "ok",
    "error",
    "completed",
    "cancelled",
    "timeout",
    "rejected",
];
const RECORD_KEYS: &[&str] = &[
    "schemaVersion",
    "kind",
    "operationId",
    "connectionId",
    "operation",
    "stage",
    "outcome",
    "startedUs",
    "elapsedUs",
];
const SUMMARY_KEYS: &[&str] = &[
    "schemaVersion",
    "kind",
    "accepted",
    "written",
    "dropped",
    "expired",
    "unfinished",
    "writeFailed",
];
const INTERPRETATION: &str = "Instrumented records over the recorder lifetime, including setup and cleanup; not the shared media measurement interval. Subsets are not random samples. Stage durations may overlap and must not be added to operation durations.";
const PERCENTILES: &str =
    "nearest-rank from individual elapsedUs values, converted to milliseconds";

type Fields = BTreeMap<String, Box<RawValue>>;

/// Availability and completeness remain separate from workload and shutdown.
#[derive(Debug, Serialize)]
pub struct Coverage {
    available: bool,
    /// True only for valid nonempty input with a consistent lossless summary.
    pub complete: bool,
    scope: &'static str,
    issues: Vec<&'static str>,
}

/// Only a validated recorder summary can appear in the output.
#[derive(Debug, Serialize)]
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    operation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    stage: Option<String>,
    outcome: String,
    count: usize,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
}

/// Sanitized aggregate report compatible with diagnostic-report.mjs. IDs, paths
/// and input text are deliberately absent, including when parsing fails.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticReport {
    schema_version: u8,
    /// Callers must retain incomplete reports and judge their coverage explicitly.
    pub coverage: Coverage,
    records: u64,
    summary: Option<Summary>,
    outcome_counts: OutcomeCounts,
    operations: Vec<Row>,
    stages: Vec<Row>,
    interpretation: &'static str,
    percentiles: &'static str,
}

// Explicit order matches the reference's outcome enumeration.
#[derive(Debug, Default, Serialize)]
struct OutcomeCounts {
    ok: u64,
    error: u64,
    completed: u64,
    cancelled: u64,
    timeout: u64,
    rejected: u64,
}

struct Record {
    kind: String,
    operation: String,
    stage: Option<String>,
    outcome: String,
    elapsed_us: u64,
}

struct Group {
    record: Record,
    values: Vec<u64>,
}

struct Reader {
    report: DiagnosticReport,
    groups: BTreeMap<String, Group>,
    terminal: bool,
    max_records: usize,
}

impl Reader {
    fn new(max_records: usize) -> Self {
        Self {
            report: DiagnosticReport {
                schema_version: 1,
                coverage: Coverage {
                    available: false,
                    complete: false,
                    scope: "unavailable",
                    issues: Vec::new(),
                },
                records: 0,
                summary: None,
                outcome_counts: OutcomeCounts::default(),
                operations: Vec::new(),
                stages: Vec::new(),
                interpretation: INTERPRETATION,
                percentiles: PERCENTILES,
            },
            groups: BTreeMap::new(),
            terminal: false,
            max_records,
        }
    }

    fn issue(&mut self, issue: &'static str) {
        if !self.report.coverage.issues.contains(&issue) {
            self.report.coverage.issues.push(issue);
        }
    }

    fn consume(&mut self, line: &[u8]) -> bool {
        if self.terminal {
            self.issue("records_after_summary");
            return false;
        }
        if line.len() > LINE_BYTES {
            self.issue("line_limit");
            return false;
        }
        let Ok(line) = std::str::from_utf8(line) else {
            self.issue("invalid_json");
            return false;
        };
        // TextDecoder's default strips one leading UTF-8 BOM on every line.
        let line = line.strip_prefix('\u{feff}').unwrap_or(line);
        if serde_json::from_str::<Box<RawValue>>(line).is_err() {
            self.issue("invalid_json");
            return false;
        }
        let fields = fields(line);
        if fields
            .as_ref()
            .and_then(|fields| string(fields, "kind"))
            .as_deref()
            == Some("summary")
        {
            let Some(summary) = fields.as_ref().and_then(summary) else {
                self.issue("invalid_summary");
                return false;
            };
            self.report.summary = Some(summary);
            self.terminal = true;
            return true;
        }
        let Some(record) = fields.as_ref().and_then(record) else {
            self.issue("invalid_record");
            return false;
        };
        if self.report.records == self.max_records as u64 {
            self.issue("record_limit");
            return false;
        }
        self.report.records += 1;
        let counts = &mut self.report.outcome_counts;
        match record.outcome.as_str() {
            "ok" => counts.ok += 1,
            "error" => counts.error += 1,
            "completed" => counts.completed += 1,
            "cancelled" => counts.cancelled += 1,
            "timeout" => counts.timeout += 1,
            "rejected" => counts.rejected += 1,
            _ => unreachable!("validated outcome vocabulary"),
        }
        let key = format!(
            "{}/{}/{}/{}",
            record.kind,
            record.operation,
            record.stage.as_deref().unwrap_or(""),
            record.outcome
        );
        let value = record.elapsed_us;
        self.groups
            .entry(key)
            .or_insert_with(|| Group {
                record,
                values: Vec::new(),
            })
            .values
            .push(value);
        true
    }

    fn read_file(
        &mut self,
        file: &mut File,
        checkpoint: &mut impl FnMut() -> bool,
        admitted: Option<u64>,
    ) -> std::io::Result<()> {
        let mut pending = Vec::new();
        let mut buffer = [0; 64 * 1024];
        let mut bytes_read = 0;
        loop {
            if !checkpoint() {
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            let remaining = admitted
                .map(|length| length.saturating_sub(bytes_read as u64))
                .unwrap_or(buffer.len() as u64);
            if remaining == 0 {
                break;
            }
            let limit = remaining.min(buffer.len() as u64) as usize;
            let count = file.read(&mut buffer[..limit])?;
            if count == 0 {
                break;
            }
            bytes_read += count;
            if bytes_read > (self.max_records + 1) * (LINE_BYTES + 1) {
                self.issue("file_limit");
                return Ok(());
            }
            pending.extend_from_slice(&buffer[..count]);
            let mut start = 0;
            while let Some(end) = pending[start..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|offset| offset + start)
            {
                if !checkpoint() {
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                if !self.consume(&pending[start..end]) {
                    return Ok(());
                }
                start = end + 1;
            }
            pending.drain(..start);
            if pending.len() > LINE_BYTES {
                self.issue("line_limit");
                return Ok(());
            }
        }
        if !pending.is_empty() {
            self.issue("unterminated_line");
        }
        Ok(())
    }

    fn finish(mut self) -> DiagnosticReport {
        if let Some(summary) = &self.report.summary {
            let mismatched =
                summary.written != self.report.records || summary.accepted != summary.written;
            let write_failed = summary.write_failed;
            let dropped = summary.dropped != 0;
            let expired = summary.expired != 0;
            let unfinished = summary.unfinished != 0;
            if mismatched {
                self.issue("count_mismatch");
            }
            if write_failed {
                self.issue("write_failed");
            }
            if dropped {
                self.issue("records_dropped");
            }
            if expired {
                self.issue("records_expired");
            }
            if unfinished {
                self.issue("unfinished_timers");
            }
        } else {
            self.issue("missing_summary");
        }
        if self.report.records == 0 && self.report.coverage.available {
            self.issue("no_records");
        }
        self.report.coverage.complete =
            self.report.coverage.available && self.report.coverage.issues.is_empty();
        self.report.coverage.scope = if !self.report.coverage.available {
            "unavailable"
        } else if self.report.coverage.complete {
            "all_emitted_records"
        } else {
            "recorded_subset"
        };
        // Every group key is fixed ASCII vocabulary. Its byte order agrees with
        // the reference localeCompare ordering for this closed set of keys.
        for (_, mut group) in self.groups {
            group.values.sort_unstable();
            let percentile = |fraction: f64| {
                group.values[(group.values.len() as f64 * fraction).ceil() as usize - 1] as f64
                    / 1000.0
            };
            let row = Row {
                operation: group.record.operation,
                stage: group.record.stage,
                outcome: group.record.outcome,
                count: group.values.len(),
                p50_ms: percentile(0.5),
                p95_ms: percentile(0.95),
                p99_ms: percentile(0.99),
            };
            if group.record.kind == "stage" {
                self.report.stages.push(row);
            } else {
                self.report.operations.push(row);
            }
        }
        self.report
    }
}

fn exact_keys(fields: &Fields, keys: &[&str]) -> bool {
    fields.len() == keys.len() && keys.iter().all(|key| fields.contains_key(*key))
}

/// JSON.parse permits unpaired UTF-16 surrogate escapes in object keys, while
/// Rust strings cannot represent them. Such a key cannot match our ASCII schema
/// but must not hide a valid kind=summary discriminator. The normal parser owns
/// syntax validation above; its raw-value stream also locates fallback entries.
fn fields(line: &str) -> Option<Fields> {
    if let Ok(fields) = serde_json::from_str(line) {
        return Some(fields);
    }
    let mut remaining = line.trim_start().strip_prefix('{')?;
    let mut fields = Fields::new();
    loop {
        remaining = remaining.trim_start();
        if remaining.starts_with('}') {
            return Some(fields);
        }
        let mut keys = serde_json::Deserializer::from_str(remaining).into_iter::<Box<RawValue>>();
        let key = keys.next()?.ok()?;
        remaining = remaining[keys.byte_offset()..]
            .trim_start()
            .strip_prefix(':')?
            .trim_start();
        let mut values = serde_json::Deserializer::from_str(remaining).into_iter::<Box<RawValue>>();
        let value = values.next()?.ok()?;
        remaining = remaining[values.byte_offset()..].trim_start();
        // This sentinel is outside the schema, so any unrepresentable key
        // guarantees an exact-key failure without retaining or echoing it.
        let key = serde_json::from_str(key.get()).unwrap_or_else(|_| "\0".to_owned());
        fields.insert(key, value);
        if let Some(rest) = remaining.strip_prefix(',') {
            remaining = rest;
        } else if !remaining.starts_with('}') {
            return None;
        }
    }
}

fn string(fields: &Fields, key: &str) -> Option<String> {
    serde_json::from_str(fields.get(key)?.get()).ok()
}

/// Parse the original JSON numeric token using the standard correctly rounded
/// float parser, then apply JavaScript's nonnegative safe-integer predicate.
/// RawValue also keeps overflow like 1e400 an invalid field, not invalid JSON.
fn integer(fields: &Fields, key: &str) -> Option<u64> {
    let value: f64 = fields.get(key)?.get().parse().ok()?;
    (value.is_finite() && (0.0..=MAX_SAFE_INTEGER).contains(&value) && value.fract() == 0.0)
        .then_some(value as u64)
}

fn record(fields: &Fields) -> Option<Record> {
    if !exact_keys(fields, RECORD_KEYS) || integer(fields, "schemaVersion")? != 1 {
        return None;
    }
    let kind = string(fields, "kind")?;
    let stage = if fields.get("stage")?.get() == "null" {
        None
    } else {
        Some(string(fields, "stage")?)
    };
    if !(kind == "operation" && stage.is_none()
        || kind == "stage"
            && stage
                .as_deref()
                .is_some_and(|value| STAGES.contains(&value)))
    {
        return None;
    }
    if integer(fields, "operationId")? == 0
        || (fields.get("connectionId")?.get() != "null" && integer(fields, "connectionId")? == 0)
    {
        return None;
    }
    let operation = string(fields, "operation")?;
    let outcome = string(fields, "outcome")?;
    if !OPERATIONS.contains(&operation.as_str()) || !OUTCOMES.contains(&outcome.as_str()) {
        return None;
    }
    integer(fields, "startedUs")?;
    Some(Record {
        kind,
        operation,
        stage,
        outcome,
        elapsed_us: integer(fields, "elapsedUs")?,
    })
}

fn summary(fields: &Fields) -> Option<Summary> {
    if !exact_keys(fields, SUMMARY_KEYS) || integer(fields, "schemaVersion")? != 1 {
        return None;
    }
    let accepted = integer(fields, "accepted")?;
    let written = integer(fields, "written")?;
    if accepted > MAX_RECORDS as u64 || written > accepted {
        return None;
    }
    Some(Summary {
        schema_version: 1,
        kind: "summary",
        accepted,
        written,
        dropped: integer(fields, "dropped")?,
        expired: integer(fields, "expired")?,
        unfinished: integer(fields, "unfinished")?,
        write_failed: serde_json::from_str(fields.get("writeFailed")?.get()).ok()?,
    })
}

/// Read only after the owned server has stopped. Invalid or missing input is
/// represented by fixed coverage issues, with valid prefix aggregates retained.
/// An invalid caller-supplied record bound is the only returned error.
pub fn read(path: &Path, max_records: usize) -> Result<DiagnosticReport, &'static str> {
    if !(1..=MAX_RECORDS).contains(&max_records) {
        return Err("Invalid diagnostic record bound");
    }
    let mut reader = Reader::new(max_records);
    match open(
        path,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    ) {
        Err(nix::errno::Errno::ENOENT) => reader.issue("file_unavailable"),
        Err(nix::errno::Errno::ELOOP) => reader.issue("unsafe_file"),
        Err(_) => reader.issue("file_unreadable"),
        Ok(fd) => {
            read_into(
                &mut reader,
                File::from(fd),
                max_records,
                &mut || true,
                false,
            );
        }
    }
    Ok(reader.finish())
}

/// Parse a caller-retained stopped recorder without reopening its pathname.
/// The file is consumed and explicitly closed on every returned path. The
/// caller separately binds original/visible metadata and writer retirement.
pub(crate) fn read_owned(
    file: File,
    max_records: usize,
    mut checkpoint: impl FnMut() -> bool,
) -> Result<DiagnosticReport, &'static str> {
    if !(1..=MAX_RECORDS).contains(&max_records) {
        let _ = nix::unistd::close(file);
        return Err("Invalid diagnostic record bound");
    }
    let mut reader = Reader::new(max_records);
    read_into(&mut reader, file, max_records, &mut checkpoint, true);
    Ok(reader.finish())
}

fn read_into(
    reader: &mut Reader,
    mut file: File,
    max_records: usize,
    checkpoint: &mut impl FnMut() -> bool,
    clip_initial: bool,
) {
    match file.metadata() {
        Err(_) => reader.issue("file_unreadable"),
        Ok(info) if !info.is_file() || info.mode() & 0o077 != 0 => reader.issue("unsafe_file"),
        Ok(info) if info.len() > ((max_records + 1) * (LINE_BYTES + 1)) as u64 => {
            reader.issue("file_limit")
        }
        Ok(info) => {
            reader.report.coverage.available = true;
            let admitted = clip_initial.then_some(info.len());
            if reader.read_file(&mut file, checkpoint, admitted).is_err() {
                reader.issue("file_unreadable");
            }
        }
    }
    if nix::unistd::close(file).is_err() {
        reader.issue("file_close_failed");
    }
}
