//! Bounded lifecycle evidence from a stopped owned server. Fixed marker fields
//! correlate anonymous ordinals and previously observed aggregate state; they do
//! not establish native causality, successful workload delivery or server exit.

use nix::fcntl::{OFlag, open};
use nix::sys::stat::Mode;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, Metadata};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

#[path = "lifecycle_records.rs"]
mod records;
#[path = "lifecycle_timeline.rs"]
mod timeline;

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;

use records::{Event, Marker};
use timeline::{Instant, Issues, View};

/// Whole-log byte ceiling, independent of line and relevant-record limits.
pub const MAX_BYTES: usize = 32 * 1024 * 1024;
/// Every observed log line counts, including ignored legacy lines.
pub const MAX_LINES: usize = 100_000;
/// Relevant valid and invalid marker/clamp lines share this ceiling.
pub const MAX_RECORDS: usize = 10_000;
/// Log bytes before a newline, including formatter escapes and private fields.
pub const LINE_BYTES: usize = 16 * 1024;
/// Timeline JSON crosses stdin rather than argv or an extra retained artifact.
pub const TIMELINE_BYTES: usize = timeline::MAX_TIMELINE_BYTES;

const INTERPRETATION: &str = "Coverage means expected instrumented lifecycle markers, not comprehensive native capture or successful server exit. Measurement boundaries are nominal wall-clock anchors, not individual media activity. Rows retain log observation order; asynchronous callbacks and formatter ordering can differ from underlying state order. Clamps contain only previously observed aggregate state, never a transport identity or causal attribution. Small timestamp reordering (up to 250ms) is tolerated; larger regressions make coverage incomplete. Explicit leave completion alone does not establish media cleanup.";

/// Availability and expected marker coverage are separate from server success.
#[derive(Debug, Serialize)]
pub struct Coverage {
    available: bool,
    /// True only when all expected markers, phases and consistency checks pass.
    pub complete: bool,
    scope: &'static str,
    issues: Vec<&'static str>,
}

/// Sanitized lifecycle report. No UUID, raw line, path or arbitrary string is
/// retained; at values are calendar-checked UTC instants from the reference.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleReport {
    schema_version: u8,
    /// Incomplete reports retain prefix evidence for the caller's judgement.
    pub coverage: Coverage,
    timeline: timeline::Output,
    lines_read: usize,
    relevant_records: usize,
    lifecycle_events: Vec<LifecycleEvent>,
    clamps: Vec<Clamp>,
    phases: Phases,
    totals: Totals,
    interpretation: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LifecycleEvent {
    ordinal: usize,
    at: String,
    phase: &'static str,
    event: &'static str,
    participant_ordinal: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport_ordinal: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport_type: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cleanup_ordinal: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Clamp {
    ordinal: usize,
    at: String,
    phase: &'static str,
    signature: &'static str,
    previously_observed: Totals,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct PhaseCounts {
    lifecycle_events: usize,
    clamps: usize,
}

#[derive(Debug, Default, Serialize)]
struct Phases {
    setup: PhaseCounts,
    measurement: PhaseCounts,
    departure: PhaseCounts,
    post_cleanup: PhaseCounts,
    shutdown: PhaseCounts,
    unknown: PhaseCounts,
}

impl Phases {
    fn counts(&mut self, phase: &str) -> &mut PhaseCounts {
        match phase {
            "setup" => &mut self.setup,
            "measurement" => &mut self.measurement,
            "departure" => &mut self.departure,
            "post_cleanup" => &mut self.post_cleanup,
            "shutdown" => &mut self.shutdown,
            _ => &mut self.unknown,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Totals {
    participants_observed: usize,
    transports_created: usize,
    transports_closed: usize,
    transports_open: usize,
    open_transports_ice_disconnected: usize,
    open_transports_dtls_failed_or_closed: usize,
    open_transports_with_disconnect_state: usize,
    ice_disconnected_events: usize,
    cleanup_generations_started: usize,
    cleanup_generations_finished: usize,
    cleanup_generations_in_progress: usize,
    participants_with_cleanup_finished: usize,
    explicit_leaves_finished: usize,
    grace_cleanups_started: usize,
}

struct Participant {
    ordinal: usize,
    markers: BTreeSet<&'static str>,
}

struct Transport {
    ordinal: usize,
    owner: String,
    kind: &'static str,
    created: bool,
    closed: bool,
    ice_ready: bool,
    dtls_ready: bool,
    ice: Option<&'static str>,
    dtls: Option<&'static str>,
}

struct Cleanup {
    ordinal: usize,
    owner: String,
    started: bool,
    finished: bool,
}

struct Reader {
    view: View,
    issues: Issues,
    expected: Option<f64>,
    available: bool,
    lines: usize,
    relevant: usize,
    last_timestamp: Option<i128>,
    disconnected_events: usize,
    participants: BTreeMap<String, Participant>,
    transports: BTreeMap<String, Transport>,
    cleanups: BTreeMap<String, Cleanup>,
    events: Vec<LifecycleEvent>,
    clamps: Vec<Clamp>,
    phases: Phases,
}

impl Reader {
    fn new(input: &str, expected: Option<f64>) -> Self {
        let mut issues = Issues::new();
        let view = View::parse(input, &mut issues);
        if expected.is_none_or(|value| {
            !value.is_finite() || value.fract() != 0.0 || !(1.0..=10000.0).contains(&value)
        }) {
            issues.insert("invalid_expected_participants");
        }
        Self {
            view,
            issues,
            expected,
            available: false,
            lines: 0,
            relevant: 0,
            last_timestamp: None,
            disconnected_events: 0,
            participants: BTreeMap::new(),
            transports: BTreeMap::new(),
            cleanups: BTreeMap::new(),
            events: Vec::new(),
            clamps: Vec::new(),
            phases: Phases::default(),
        }
    }

    fn totals(&self) -> Totals {
        let open: Vec<_> = self
            .transports
            .values()
            .filter(|row| row.created && !row.closed)
            .collect();
        let disconnected = |row: &&Transport| row.ice == Some("Disconnected");
        let failed = |row: &&Transport| matches!(row.dtls, Some("Failed" | "Closed"));
        Totals {
            participants_observed: self.participants.len(),
            transports_created: self.transports.values().filter(|row| row.created).count(),
            transports_closed: self.transports.values().filter(|row| row.closed).count(),
            transports_open: open.len(),
            open_transports_ice_disconnected: open.iter().filter(|row| disconnected(row)).count(),
            open_transports_dtls_failed_or_closed: open.iter().filter(|row| failed(row)).count(),
            open_transports_with_disconnect_state: open
                .iter()
                .filter(|row| disconnected(row) || failed(row))
                .count(),
            ice_disconnected_events: self.disconnected_events,
            cleanup_generations_started: self.cleanups.values().filter(|row| row.started).count(),
            cleanup_generations_finished: self.cleanups.values().filter(|row| row.finished).count(),
            cleanup_generations_in_progress: self
                .cleanups
                .values()
                .filter(|row| row.started && !row.finished)
                .count(),
            participants_with_cleanup_finished: self
                .cleanups
                .values()
                .filter(|row| row.started && row.finished)
                .map(|row| &row.owner)
                .collect::<BTreeSet<_>>()
                .len(),
            explicit_leaves_finished: self
                .participants
                .values()
                .filter(|row| row.markers.contains("explicit_leave_finished"))
                .count(),
            grace_cleanups_started: self
                .participants
                .values()
                .filter(|row| row.markers.contains("grace_cleanup_started"))
                .count(),
        }
    }

    fn consume(&mut self, bytes: &[u8]) -> bool {
        self.lines += 1;
        if self.lines > MAX_LINES {
            self.issues.insert("line_count_limit");
            return false;
        }
        if bytes.len() > LINE_BYTES {
            self.issues.insert("line_length_limit");
            return false;
        }
        let line = match records::decode(bytes) {
            Ok(line) => line,
            Err(issue) => {
                self.issues.insert(issue);
                return true;
            }
        };
        let Some(lifecycle) = records::relevant(&line) else {
            return true;
        };
        if records::has_control(&line) {
            self.issues.insert("invalid_log_control");
            return true;
        }
        if self.relevant >= MAX_RECORDS {
            self.issues.insert("record_limit");
            return false;
        }
        self.relevant += 1;
        let Some((time, event)) = records::parse(&line, lifecycle) else {
            self.issues.insert("invalid_lifecycle_record");
            return true;
        };
        if self
            .last_timestamp
            .is_some_and(|previous| previous - time.ns > 250_000_000)
        {
            self.issues.insert("log_clock_regression");
        }
        self.last_timestamp = Some(
            self.last_timestamp
                .map_or(time.ns, |previous| previous.max(time.ns)),
        );
        let phase = self.view.phase(&time);
        if phase == "unknown" {
            self.issues.insert("unknown_record_phase");
        }
        match event {
            Event::Clamp => {
                let totals = self.totals();
                self.clamps.push(Clamp {
                    ordinal: self.clamps.len() + 1,
                    at: time.at,
                    phase,
                    signature: "start_bitrate_below_minimum_30000_100000",
                    previously_observed: totals,
                });
                self.phases.counts(phase).clamps += 1;
            }
            Event::Lifecycle(marker) => self.marker(time, phase, marker),
        }
        true
    }

    fn marker(&mut self, time: Instant, phase: &'static str, marker: Marker) {
        let (event, owner) = match &marker {
            Marker::Transport { event, owner, .. }
            | Marker::Cleanup { event, owner, .. }
            | Marker::Participant { event, owner } => (*event, owner),
        };
        let next = self.participants.len() + 1;
        let participant = self
            .participants
            .entry(owner.clone())
            .or_insert_with(|| Participant {
                ordinal: next,
                markers: BTreeSet::new(),
            });
        let mut row = LifecycleEvent {
            ordinal: self.events.len() + 1,
            at: time.at,
            phase,
            event,
            participant_ordinal: participant.ordinal,
            transport_ordinal: None,
            transport_type: None,
            state: None,
            cleanup_ordinal: None,
        };
        match marker {
            Marker::Transport {
                event,
                owner,
                id,
                kind,
                state,
            } => {
                let next = self.transports.len() + 1;
                let transport = self.transports.entry(id).or_insert_with(|| Transport {
                    ordinal: next,
                    owner: owner.clone(),
                    kind,
                    created: false,
                    closed: false,
                    ice_ready: false,
                    dtls_ready: false,
                    ice: None,
                    dtls: None,
                });
                if transport.owner != owner || transport.kind != kind {
                    self.issues.insert("transport_identity_conflict");
                    return;
                }
                row.transport_ordinal = Some(transport.ordinal);
                row.transport_type = Some(kind);
                if event == "transport_created" {
                    if transport.created {
                        self.issues.insert("duplicate_transport_created");
                    }
                    transport.created = true;
                }
                if event == "transport_closed" {
                    if transport.closed {
                        self.issues.insert("duplicate_transport_closed");
                    }
                    transport.closed = true;
                }
                if let Some(state) = state {
                    row.state = Some(state);
                    if event == "transport_ice" {
                        transport.ice = Some(state);
                        if matches!(state, "Connected" | "Completed") {
                            transport.ice_ready = true;
                        }
                        if state == "Disconnected" {
                            self.disconnected_events += 1;
                        }
                    } else {
                        transport.dtls = Some(state);
                        if state == "Connected" {
                            transport.dtls_ready = true;
                        }
                    }
                }
            }
            Marker::Cleanup {
                event,
                owner,
                generation,
            } => {
                let next = self.cleanups.len() + 1;
                let cleanup = self.cleanups.entry(generation).or_insert_with(|| Cleanup {
                    ordinal: next,
                    owner: owner.clone(),
                    started: false,
                    finished: false,
                });
                if cleanup.owner != owner {
                    self.issues.insert("cleanup_identity_conflict");
                    return;
                }
                let flag = if event == "media_cleanup_started" {
                    &mut cleanup.started
                } else {
                    &mut cleanup.finished
                };
                if *flag {
                    self.issues.insert("duplicate_cleanup_marker");
                }
                *flag = true;
                row.cleanup_ordinal = Some(cleanup.ordinal);
            }
            Marker::Participant { event, owner } => {
                self.participants
                    .get_mut(&owner)
                    .expect("observed participant")
                    .markers
                    .insert(event);
            }
        }
        self.events.push(row);
        self.phases.counts(phase).lifecycle_events += 1;
    }

    fn read_file(&mut self, file: &mut File) -> std::io::Result<()> {
        let mut pending = Vec::new();
        let mut buffer = [0; 64 * 1024];
        let mut bytes = 0;
        let mut halted = false;
        while !halted && bytes < MAX_BYTES {
            let count = file.read(&mut buffer[..(MAX_BYTES - bytes).min(64 * 1024)])?;
            if count == 0 {
                break;
            }
            bytes += count;
            pending.extend_from_slice(&buffer[..count]);
            let mut start = 0;
            while let Some(end) = pending[start..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|offset| offset + start)
            {
                if !self.consume(&pending[start..end]) {
                    halted = true;
                    break;
                }
                start = end + 1;
            }
            if !halted {
                pending.drain(..start);
            }
            if !halted && pending.len() > LINE_BYTES {
                self.issues.insert("line_length_limit");
                halted = true;
            }
        }
        if !halted && !pending.is_empty() {
            self.issues.insert("unterminated_line");
        }
        Ok(())
    }

    fn finish(mut self) -> LifecycleReport {
        let totals = self.totals();
        if self.events.is_empty() {
            self.issues.insert("missing_lifecycle_markers");
        }
        if self
            .expected
            .is_some_and(|expected| (totals.participants_with_cleanup_finished as f64) < expected)
        {
            self.issues.insert("expected_participant_cleanup_missing");
        }
        for transport in self.transports.values() {
            if !transport.created {
                self.issues.insert("transport_created_missing");
            }
            if !transport.closed {
                self.issues.insert("transport_closed_missing");
            }
            if transport.created && !transport.ice_ready {
                self.issues.insert("transport_ice_ready_missing");
            }
            if transport.created && !transport.dtls_ready {
                self.issues.insert("transport_dtls_ready_missing");
            }
        }
        for cleanup in self.cleanups.values() {
            if !cleanup.started || !cleanup.finished {
                self.issues.insert("cleanup_marker_missing");
            }
        }
        for (id, owner) in &self.participants {
            if !self
                .transports
                .values()
                .any(|row| row.owner == *id && row.kind == "recv" && row.created)
            {
                self.issues.insert("participant_recv_transport_missing");
            }
            if !self
                .cleanups
                .values()
                .any(|row| row.owner == *id && row.started && row.finished)
            {
                self.issues.insert("participant_cleanup_missing");
            }
            if self.view.output.departure == Some("explicit-leave")
                && (!owner.markers.contains("explicit_leave_started")
                    || !owner.markers.contains("explicit_leave_finished"))
            {
                self.issues.insert("explicit_leave_marker_missing");
            }
            if self.view.output.departure == Some("abrupt")
                && (!owner.markers.contains("grace_started")
                    || !owner.markers.contains("grace_cleanup_started"))
            {
                self.issues.insert("grace_cleanup_marker_missing");
            }
        }
        let complete = self.available && self.issues.is_empty();
        LifecycleReport {
            schema_version: 1,
            coverage: Coverage {
                available: self.available,
                complete,
                scope: if !self.available {
                    "unavailable"
                } else if complete {
                    "expected_instrumented_markers"
                } else {
                    "observed_subset"
                },
                issues: self.issues.into_iter().collect(),
            },
            timeline: self.view.output,
            lines_read: self.lines,
            relevant_records: self.relevant,
            lifecycle_events: self.events,
            clamps: self.clamps,
            phases: self.phases,
            totals,
            interpretation: INTERPRETATION,
        }
    }
}

/// Read after the owned server stops. Invalid timeline JSON and unavailable log
/// files become fixed coverage issues; valid prefix evidence remains available.
/// Timeline input is additionally bounded even for direct library callers.
pub fn read(path: &Path, timeline: &str, expected_participants: Option<f64>) -> LifecycleReport {
    let mut reader = Reader::new(timeline, expected_participants);
    match open(
        path,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    ) {
        Err(nix::errno::Errno::ENOENT) => {
            reader.issues.insert("file_unavailable");
        }
        Err(nix::errno::Errno::ELOOP) => {
            reader.issues.insert("unsafe_file");
        }
        Err(_) => {
            reader.issues.insert("file_unreadable");
        }
        Ok(fd) => {
            let mut file = File::from(fd);
            match file.metadata() {
                Err(_) => {
                    reader.issues.insert("file_unreadable");
                }
                Ok(info) if !info.is_file() || info.mode() & 0o077 != 0 => {
                    reader.issues.insert("unsafe_file");
                }
                Ok(info) => {
                    reader.available = true;
                    if info.len() > MAX_BYTES as u64 {
                        reader.issues.insert("file_limit");
                    }
                    if reader.read_file(&mut file).is_err() {
                        reader.issues.insert("file_unreadable");
                    } else {
                        match file.metadata() {
                            Err(_) => {
                                reader.issues.insert("file_unreadable");
                            }
                            Ok(after) => {
                                if after.len() > MAX_BYTES as u64 {
                                    reader.issues.insert("file_limit");
                                }
                                if changed(&info, &after) {
                                    reader.issues.insert("log_changed_during_read");
                                }
                            }
                        }
                    }
                }
            }
            if nix::unistd::close(file).is_err() {
                reader.issues.insert("file_close_failed");
            }
        }
    }
    reader.finish()
}

fn changed(before: &Metadata, after: &Metadata) -> bool {
    // Match Node stat's millisecond precision rather than inventing a stricter
    // nanosecond race policy than the reference reports.
    let mtime =
        |value: &Metadata| value.mtime() as f64 * 1000.0 + value.mtime_nsec() as f64 / 1_000_000.0;
    let ctime =
        |value: &Metadata| value.ctime() as f64 * 1000.0 + value.ctime_nsec() as f64 / 1_000_000.0;
    before.len() != after.len() || mtime(before) != mtime(after) || ctime(before) != ctime(after)
}
