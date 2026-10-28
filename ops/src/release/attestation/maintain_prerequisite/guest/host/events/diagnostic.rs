//! Bounded passive event failures. Commands become exact-byte hashes only.

use super::{AuditFailure, Error};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy)]
pub(super) enum Stage {
    Input,
    Shape,
    Image,
    Container,
    Network,
    ActorCounts,
    ActorTraces,
    NetworkTraces,
    Pg,
    InterruptionOrder,
    ProxyValidation,
    ProxyReplacement,
    ColdPg,
}
impl Stage {
    pub fn checkpoint(self) -> &'static str {
        match self {
            Self::Input => "host-event-input",
            Self::Shape => "host-event-shape",
            Self::Image => "host-event-image",
            Self::Container => "host-event-container",
            Self::Network => "host-event-network",
            Self::ActorCounts => "host-event-actor-counts",
            Self::ActorTraces => "host-event-actor-traces",
            Self::NetworkTraces => "host-event-network-traces",
            Self::Pg => "host-event-pg",
            Self::InterruptionOrder => "host-event-interruption-order",
            Self::ProxyValidation => "host-event-proxy-validation",
            Self::ProxyReplacement => "host-event-proxy-replacement",
            Self::ColdPg => "daemon-pg-history",
        }
    }
}

#[derive(Clone, Copy)]
pub(in super::super::super) enum PgReason {
    SourceMissing,
    SourceUtf8,
    QueryShape,
    QueryCount,
    ActionShape,
    ExecId,
    DuplicateCreate,
    UnknownCommand,
    MissingCreate,
    StartOrder,
    DieAction,
    DieOrder,
    NonzeroExit,
    IncompleteSequence,
    CommandCount,
}
impl PgReason {
    pub fn name(self) -> &'static str {
        match self {
            Self::SourceMissing => "pg-source-missing",
            Self::SourceUtf8 => "pg-source-utf8",
            Self::QueryShape => "pg-query-shape",
            Self::QueryCount => "pg-query-count",
            Self::ActionShape => "pg-action-shape",
            Self::ExecId => "pg-exec-id",
            Self::DuplicateCreate => "pg-duplicate-create",
            Self::UnknownCommand => "pg-unknown-command",
            Self::MissingCreate => "pg-missing-create",
            Self::StartOrder => "pg-start-order",
            Self::DieAction => "pg-die-action",
            Self::DieOrder => "pg-die-order",
            Self::NonzeroExit => "pg-nonzero-exit",
            Self::IncompleteSequence => "pg-incomplete-sequence",
            Self::CommandCount => "pg-command-count",
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Class {
    Shape,
    Identity,
    Ordering,
    Cardinality,
    UnknownCommand,
    NonzeroExit,
    IncompleteSequence,
    Pg(PgReason),
}
impl Serialize for Class {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            Self::Shape => "shape",
            Self::Identity => "identity",
            Self::Ordering => "ordering",
            Self::Cardinality => "cardinality",
            Self::UnknownCommand => "unknown-command",
            Self::NonzeroExit => "nonzero-exit",
            Self::IncompleteSequence => "incomplete-sequence",
            Self::Pg(reason) => reason.name(),
        })
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    schema_version: u8,
    scope: &'static str,
    stage: &'static str,
    failure_class: Class,
    row_index: Option<usize>,
    rows: usize,
    expected_command_sha256: Option<String>,
    observed_command_sha256: Option<String>,
    expected_count: Option<usize>,
    observed_count: Option<usize>,
}

pub(in super::super::super) struct Auditor<'a> {
    pub(super) stage: Stage,
    raw: &'a [Value],
    cursor: usize,
    row: Option<usize>,
    detail: Option<Value>,
}
impl<'a> Auditor<'a> {
    pub fn new(raw: &'a [Value]) -> Self {
        Self {
            stage: Stage::Input,
            raw,
            cursor: 0,
            row: None,
            detail: None,
        }
    }
    pub fn begin_pg(&mut self) {
        self.stage = Stage::Pg;
        self.cursor = 0;
        self.row = None;
    }
    pub fn begin_cold_pg(&mut self) {
        self.stage = Stage::ColdPg;
        self.cursor = 0;
        self.row = None;
    }
    pub fn diagnostic(self) -> Option<Value> {
        self.detail
    }
    pub fn observe(&mut self, row: &Value) {
        // Adapters clone only their unchanged retained rows. A forward
        // join preserves their original captured positions, even with repeated
        // equal JSON records; no reported actor identifier authorizes a lookup.
        self.row = self.raw[self.cursor..]
            .iter()
            .position(|raw| raw == row)
            .map(|relative| self.cursor + relative);
        if let Some(index) = self.row {
            self.cursor = index + 1;
        }
    }
    pub fn post_rows(&mut self) {
        self.row = None;
    }
    pub(super) fn note(
        &mut self,
        class: Class,
        expected_command: Option<&str>,
        observed_command: Option<&str>,
        expected_count: Option<usize>,
        observed_count: Option<usize>,
    ) {
        if self.detail.is_some() || self.raw.len() > 2048 {
            return;
        }
        let hash =
            |command: &str| super::super::super::files::hex(&Sha256::digest(command.as_bytes()));
        let record = Record {
            schema_version: 1,
            scope: "PreparedHostObservationDiagnostic",
            stage: self.stage.checkpoint(),
            failure_class: class,
            row_index: self
                .row
                .filter(|index| *index < self.raw.len() && *index < 2048),
            rows: self.raw.len(),
            expected_command_sha256: expected_command.map(hash),
            observed_command_sha256: observed_command.map(hash),
            expected_count: expected_count.filter(|count| *count <= 2048),
            observed_count: observed_count.filter(|count| *count <= 2048),
        };
        // Diagnostic serialization never replaces the original failure. Every
        // field has a closed domain, and a larger record is omitted wholesale.
        if let Ok(value) = serde_json::to_value(record)
            && serde_json::to_vec(&value).is_ok_and(|bytes| bytes.len() <= 1024)
        {
            self.detail = Some(value);
        }
    }
    pub fn note_pg(
        &mut self,
        reason: PgReason,
        expected_command: Option<&str>,
        observed_command: Option<&str>,
        expected_count: Option<usize>,
        observed_count: Option<usize>,
    ) {
        self.note(
            Class::Pg(reason),
            expected_command,
            observed_command,
            expected_count,
            observed_count,
        );
    }
    pub fn failure(self, cause: Error) -> AuditFailure {
        AuditFailure {
            cause,
            checkpoint: self.stage.checkpoint(),
            diagnostic: self.detail,
        }
    }
}
