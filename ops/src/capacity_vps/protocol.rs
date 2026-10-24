//! Closed wire data for the private capacity operation. No value authenticates code.

use super::{Failure, Result};
use crate::distribution::manifest::Selection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{self, Write};

pub use simplestchat_load_test::capacity::vps::Request as Workload;
pub use simplestchat_load_test::capacity::vps::{Outcome, project_controller_report};

pub const MAX_HEADER: usize = 64 * 1024;
pub const MAX_REQUEST: usize = 512 * 1024;
pub const MAX_RESPONSE: usize = 8 * 1024 * 1024;
pub const MAX_STATUS: usize = 64 * 1024;
pub const MAX_TRANSFER: u64 = 281 * 1024 * 1024;
pub const BUILD_UNIT: &str = "simplestchat-image-build.service";
const MAX_ID: u64 = 9_007_199_254_740_991;

pub(super) fn hex(value: &str, size: usize) -> bool {
    value.len() == size
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn bounded<T: Serialize>(value: &T, maximum: usize) -> Result<Vec<u8>> {
    let mut output = BoundedWriter {
        bytes: Vec::new(),
        maximum,
        exhausted: false,
    };
    if serde_json::to_writer(&mut output, value).is_err() {
        return Err(if output.exhausted {
            Failure::Bounds
        } else {
            Failure::Framing
        });
    }
    Ok(output.bytes)
}

// Refuse each serializer write before allocating or extending beyond the cap.
struct BoundedWriter {
    bytes: Vec<u8>,
    maximum: usize,
    exhausted: bool,
}
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.maximum.saturating_sub(self.bytes.len()) {
            self.exhausted = true;
            return Err(io::Error::other("capacity_response_bound"));
        }
        let needed = self.bytes.len() + bytes.len();
        if needed > self.bytes.capacity() {
            let target = needed
                .max(self.bytes.capacity().saturating_mul(2))
                .min(self.maximum);
            if self
                .bytes
                .try_reserve_exact(target - self.bytes.len())
                .is_err()
            {
                self.exhausted = true;
                return Err(io::Error::other("capacity_response_bound"));
            }
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// These fields may be omitted, but a present null is not an omission.
fn present<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

// Using deserialize_with without default requires the key, while retaining null.
fn nullable<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8], maximum: usize) -> Result<T> {
    crate::json::decode_limited(bytes, maximum).map_err(|_| Failure::Framing)?;
    serde_json::from_slice(bytes).map_err(|_| Failure::Framing)
}

/// A byte declaration, not proof that those bytes were observed or authenticated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub sha256: String,
    pub bytes: u64,
}
impl Pin {
    pub fn validate(&self, maximum: u64) -> Result<()> {
        if hex(&self.sha256, 64) && (1..=maximum).contains(&self.bytes) {
            Ok(())
        } else {
            Err(Failure::Identity)
        }
    }
}

/// The fixed three transport members, with no path-selection field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Members {
    #[serde(rename = "ops-release.json")]
    pub manifest: Pin,
    #[serde(rename = "ops-selection.json")]
    pub selection: Pin,
    #[serde(rename = "simplestchat-ops")]
    pub binary: Pin,
}
impl Members {
    pub fn validate(&self) -> Result<()> {
        self.manifest.validate(64 * 1024)?;
        self.selection.validate(16 * 1024)?;
        self.binary.validate(crate::distribution::MAX_BYTES)
    }
}

/// Seven caller declarations validated by the existing distribution selection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Source {
    pub repository: String,
    pub revision: String,
    pub package_version: String,
    pub protocol_version: u8,
    pub target: String,
    pub build_run_id: u64,
    pub run_attempt: u64,
}
impl Source {
    pub fn validate(&self) -> Result<()> {
        if !(1..=MAX_ID).contains(&self.build_run_id) || !(1..=MAX_ID).contains(&self.run_attempt) {
            return Err(Failure::Identity);
        }
        Selection::new(
            &self.repository,
            &self.revision,
            &self.package_version,
            self.protocol_version,
            &self.target,
            self.build_run_id as i64,
            self.run_attempt as i64,
        )
        .map(|_| ())
        .map_err(|_| Failure::Identity)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Stage,
    Invoke,
}

/// Closed transport data. Construction or decoding supplies no acquisition proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Header {
    pub schema_version: u8,
    pub scope: String,
    pub mode: Mode,
    pub run: String,
    pub selection: Source,
    pub artifact_id: u64,
    pub transfer: Pin,
    pub members: Members,
    pub request: Pin,
}
impl Header {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1
            || self.scope != "OpsCapacityBootstrap"
            || !hex(&self.run, 32)
            || !(1..=MAX_ID).contains(&self.artifact_id)
        {
            return Err(Failure::Identity);
        }
        self.selection.validate()?;
        self.transfer.validate(MAX_TRANSFER)?;
        self.members.validate()?;
        self.request.validate(MAX_REQUEST as u64)
    }

    /// Decode one header value without its framing LF.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value: Self = decode(bytes, MAX_HEADER - 1)?;
        value.validate()?;
        Ok(value)
    }

    /// Encode validated data; the transport appends the one bounded header LF.
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        bounded(self, MAX_HEADER - 1)
    }

    pub fn capacity_unit(&self) -> String {
        format!("simplestchat-capacity-{}.service", self.run)
    }

    pub fn evidence_path(&self) -> String {
        format!(
            "/srv/simplestchat-bench/results/capacity-controller.{}",
            self.run
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Preflight,
    Reserve,
    Build,
    Start,
    Status,
    Lookup,
    Collect,
    Recover,
    Archive,
    Retire,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Unit {
    Build,
    Capacity,
}

/// An original workload string is retained byte-for-byte, including private tokens.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Envelope {
    schema_version: u8,
    scope: String,
    action: Action,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unit: Option<Unit>,
}
impl Envelope {
    pub fn lookup() -> Self {
        Self {
            schema_version: 1,
            scope: "OpsCapacityOperation".into(),
            action: Action::Lookup,
            request_json: None,
            unit: None,
        }
    }

    pub fn request(action: Action, original: &[u8], unit: Option<Unit>) -> Result<Self> {
        let value = Self {
            schema_version: 1,
            scope: "OpsCapacityOperation".into(),
            action,
            request_json: Some(
                std::str::from_utf8(original)
                    .map_err(|_| Failure::Request)?
                    .into(),
            ),
            unit,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != 1 || self.scope != "OpsCapacityOperation" {
            return Err(Failure::Framing);
        }
        if self.action == Action::Lookup {
            if self.request_json.is_some() || self.unit.is_some() {
                return Err(Failure::Request);
            }
        } else {
            Workload::decode(self.original_request().ok_or(Failure::Request)?)
                .map_err(|_| Failure::Request)?;
            if (self.action == Action::Status) != self.unit.is_some() {
                return Err(Failure::Request);
            }
        }
        Ok(())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value: Self = decode(bytes, MAX_REQUEST)?;
        // Explicit null is not an omitted optional field in the closed wire.
        let raw: Value = serde_json::from_slice(bytes).map_err(|_| Failure::Framing)?;
        if raw.get("requestJson").is_some_and(Value::is_null)
            || raw.get("unit").is_some_and(Value::is_null)
        {
            return Err(Failure::Request);
        }
        value.validate()?;
        Ok(value)
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        bounded(self, MAX_REQUEST)
    }

    pub fn action(&self) -> Action {
        self.action
    }

    pub fn unit(&self) -> Option<Unit> {
        self.unit
    }

    pub fn original_request(&self) -> Option<&[u8]> {
        self.request_json.as_ref().map(String::as_bytes)
    }

    pub fn workload(&self) -> Result<Workload> {
        Workload::decode(self.original_request().ok_or(Failure::Request)?)
            .map_err(|_| Failure::Request)
    }

    pub fn bind(&self, header: &Header) -> Result<()> {
        header.validate()?;
        self.validate()?;
        if self.action != Action::Lookup {
            workload(self.original_request().ok_or(Failure::Request)?, header)?;
        }
        Ok(())
    }
}

/// Observations only. Terminal or successful strings are not cleanup authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnitState {
    #[serde(rename = "LoadState")]
    pub load_state: String,
    #[serde(rename = "ActiveState")]
    pub active_state: String,
    #[serde(rename = "MainPID")]
    pub main_pid: String,
    #[serde(
        rename = "SubState",
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub sub_state: Option<String>,
    #[serde(
        rename = "Result",
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub result: Option<String>,
    #[serde(
        rename = "ExecMainCode",
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub exec_main_code: Option<String>,
    #[serde(
        rename = "ExecMainStatus",
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub exec_main_status: Option<String>,
    #[serde(
        rename = "Unit",
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub unit: Option<String>,
    #[serde(
        rename = "Retained",
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub retained: Option<bool>,
}
impl UnitState {
    pub fn terminal(&self) -> bool {
        matches!(self.active_state.as_str(), "inactive" | "failed")
            || (self.active_state == "active"
                && self.sub_state.as_deref() == Some("exited")
                && self.main_pid == "0")
    }

    pub fn successful(&self) -> bool {
        self.load_state == "loaded"
            && self.terminal()
            && self.main_pid == "0"
            && self.result.as_deref() == Some("success")
            && self.exec_main_status.as_deref() == Some("0")
    }

    pub fn validate(&self, header: &Header, selected: Unit) -> Result<()> {
        let fields = [
            Some(&self.active_state),
            Some(&self.main_pid),
            self.sub_state.as_ref(),
            self.result.as_ref(),
            self.exec_main_code.as_ref(),
            self.exec_main_status.as_ref(),
        ];
        if !matches!(self.load_state.as_str(), "loaded" | "not-found")
            || fields
                .into_iter()
                .flatten()
                .any(|v| v.len() > 1024 || !v.is_ascii() || v.contains(['\n', '\r']))
            || self.retained == Some(false)
        {
            return Err(Failure::Unit);
        }
        let expected = match selected {
            Unit::Build => BUILD_UNIT.into(),
            Unit::Capacity => header.capacity_unit(),
        };
        if self.unit.as_ref().is_some_and(|v| v != &expected)
            || self.retained == Some(true)
                && (selected != Unit::Capacity || self.unit.is_none() || !self.terminal())
        {
            return Err(Failure::Unit);
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Residuals {
    pub containers_before: Vec<String>,
    pub containers_after: Vec<String>,
    pub residuals: Vec<Residual>,
    pub clean: bool,
}
impl Residuals {
    pub(super) fn validate(&self) -> Result<()> {
        if !container_ids(&self.containers_before)
            || !container_ids(&self.containers_after)
            || self.clean != self.residuals.is_empty()
            || self
                .residuals
                .iter()
                .any(|entry| !hex(&entry.id, 64) || entry.inspection.id != entry.id)
            || self.containers_after.iter().any(|id| {
                !self.containers_before.contains(id)
                    && !self.residuals.iter().any(|entry| &entry.id == id)
            })
        {
            return Err(Failure::Collection);
        }
        Ok(())
    }
}

fn container_ids(values: &[String]) -> bool {
    values.iter().all(|value| hex(value, 64))
}

fn workload(bytes: &[u8], header: &Header) -> Result<Workload> {
    let value = Workload::decode(bytes).map_err(|_| Failure::Request)?;
    if value.run_id() != header.run || value.revision() != header.selection.revision {
        return Err(Failure::Identity);
    }
    Ok(value)
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Residual {
    pub id: String,
    pub confirmed_owned: bool,
    pub inspection: Inspection,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inspection {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Image", deserialize_with = "nullable")]
    pub image: Option<String>,
    #[serde(rename = "RequestedImage", deserialize_with = "nullable")]
    pub requested_image: Option<String>,
    #[serde(rename = "State")]
    pub state: Lifecycle,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lifecycle {
    #[serde(rename = "Status", deserialize_with = "nullable")]
    pub status: Option<String>,
    #[serde(rename = "Running", deserialize_with = "nullable")]
    pub running: Option<bool>,
    #[serde(rename = "ExitCode", deserialize_with = "nullable")]
    pub exit_code: Option<i32>,
    #[serde(rename = "OOMKilled", deserialize_with = "nullable")]
    pub oom_killed: Option<bool>,
    #[serde(rename = "StartedAt", deserialize_with = "nullable")]
    pub started_at: Option<String>,
    #[serde(rename = "FinishedAt", deserialize_with = "nullable")]
    pub finished_at: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Preflight {
    pub containers_before: Vec<String>,
    pub private_host: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Ready {
    pub private_host: bool,
    pub ready: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    pub unit: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Start {
    pub unit: String,
    pub evidence: String,
    pub run: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Lookup {
    pub request_json: String,
    pub request_sha256: String,
    pub unit: UnitState,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Collect {
    pub passed: bool,
    pub unit: UnitState,
    pub outcome_json: String,
    pub cleanup: Residuals,
    pub evidence: String,
    pub revision: String,
    pub request_json: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recovery {
    pub run: String,
    pub removed: Vec<String>,
    pub finalized: bool,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub before: Option<Residuals>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub after: Option<Residuals>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RetirementPhase {
    Prestart,
    Terminal,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Retirement {
    pub retired: bool,
    pub lifecycle: RetirementPhase,
    pub request_sha256: String,
}

/// Action-selected private data; archive is deliberately not a JSON result.
#[derive(Clone, Serialize)]
#[serde(untagged)]
pub enum ResultData {
    Preflight(Preflight),
    Reserve(Ready),
    Build(Build),
    Start(Start),
    Status(UnitState),
    Lookup(Lookup),
    Collect(Collect),
    Recover(Recovery),
    Retire(Retirement),
}

/// Decoded action result, bound to the caller's separately retained header data.
pub struct Response {
    pub result: ResultData,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireResponse {
    schema_version: u8,
    scope: String,
    action: Action,
    run: String,
    revision: String,
    artifact_id: u64,
    binary_sha256: String,
    result: Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WireResponseRef<'a> {
    schema_version: u8,
    scope: &'static str,
    action: Action,
    run: &'a str,
    revision: &'a str,
    artifact_id: u64,
    binary_sha256: &'a str,
    result: &'a ResultData,
}

impl ResultData {
    pub fn action(&self) -> Action {
        match self {
            Self::Preflight(_) => Action::Preflight,
            Self::Reserve(_) => Action::Reserve,
            Self::Build(_) => Action::Build,
            Self::Start(_) => Action::Start,
            Self::Status(_) => Action::Status,
            Self::Lookup(_) => Action::Lookup,
            Self::Collect(_) => Action::Collect,
            Self::Recover(_) => Action::Recover,
            Self::Retire(_) => Action::Retire,
        }
    }

    pub fn value(&self) -> Result<Value> {
        serde_json::from_slice(&bounded(self, response_limit(self.action()) - 1)?)
            .map_err(|_| Failure::Framing)
    }

    fn validate(&self, header: &Header) -> Result<()> {
        match self {
            Self::Preflight(value) => {
                if !value.private_host || !container_ids(&value.containers_before) {
                    return Err(Failure::HostBusy);
                }
            }
            Self::Reserve(value) => {
                if !value.private_host || !value.ready {
                    return Err(Failure::HostBusy);
                }
            }
            Self::Build(value) => {
                if value.unit != BUILD_UNIT {
                    return Err(Failure::Unit);
                }
            }
            Self::Start(value) => {
                if value.unit != header.capacity_unit()
                    || value.evidence != header.evidence_path()
                    || value.run != header.run
                {
                    return Err(Failure::Identity);
                }
            }
            Self::Status(value) => {
                // The invocation owner additionally binds the requested unit enum.
                let selected = if value.unit.as_deref() == Some(BUILD_UNIT) {
                    Unit::Build
                } else {
                    Unit::Capacity
                };
                value.validate(header, selected)?;
            }
            Self::Lookup(value) => {
                workload(value.request_json.as_bytes(), header)?;
                if value.request_sha256 != sha(value.request_json.as_bytes()) {
                    return Err(Failure::Identity);
                }
                value.unit.validate(header, Unit::Capacity)?;
            }
            Self::Collect(value) => {
                let request = workload(value.request_json.as_bytes(), header)?;
                value.unit.validate(header, Unit::Capacity)?;
                value.cleanup.validate()?;
                let outcome = Outcome::decode(value.outcome_json.as_bytes())
                    .map_err(|_| Failure::Collection)?;
                outcome
                    .validate_workload(&request)
                    .map_err(|_| Failure::Collection)?;
                if let Some(cleanup) = outcome.cleanup_json() {
                    let cleanup = decode::<Residuals>(&cleanup, MAX_RESPONSE)
                        .map_err(|_| Failure::Collection)?;
                    cleanup.validate()?;
                    // Failed workers can preserve a snapshot whose baseline file
                    // was never published. Keep that contradictory evidence;
                    // only a positive collection requires both baselines to agree.
                    if value.passed && cleanup.containers_before != value.cleanup.containers_before
                    {
                        return Err(Failure::Collection);
                    }
                }
                if value.revision != header.selection.revision
                    || value.evidence != header.evidence_path()
                    || outcome
                        .revision()
                        .is_some_and(|v| v != header.selection.revision)
                    || value.passed
                        && !(outcome.passed()
                            && outcome.finalized()
                            && value.cleanup.clean
                            && value.unit.successful())
                {
                    return Err(Failure::Collection);
                }
            }
            Self::Recover(value) => {
                if value.run != header.run
                    || !container_ids(&value.removed)
                    || value.removed.len() > 6
                {
                    return Err(Failure::Ownership);
                }
                let before = value.before.as_ref().ok_or(Failure::Collection)?;
                let after = value.after.as_ref().ok_or(Failure::Collection)?;
                before.validate()?;
                after.validate()?;
                if before.containers_before != after.containers_before
                    || before.residuals.len() > 6
                    || value.removed.iter().any(|id| {
                        !before
                            .residuals
                            .iter()
                            .any(|entry| &entry.id == id && entry.confirmed_owned)
                    })
                    || value.finalized && !after.clean
                {
                    return Err(Failure::Ownership);
                }
            }
            Self::Retire(value) => {
                if !value.retired || !hex(&value.request_sha256, 64) {
                    return Err(Failure::Retirement);
                }
            }
        }
        Ok(())
    }
}

impl Response {
    /// Encode one compact record and its mandatory LF, after the whole byte cap.
    pub fn encode(header: &Header, result: &ResultData) -> Result<Vec<u8>> {
        header.validate()?;
        result.validate(header)?;
        let wire = WireResponseRef {
            schema_version: 1,
            scope: "OpsCapacityResponse",
            action: result.action(),
            run: &header.run,
            revision: &header.selection.revision,
            artifact_id: header.artifact_id,
            binary_sha256: &header.members.binary.sha256,
            result,
        };
        let mut bytes = bounded(&wire, response_limit(wire.action) - 1)?;
        bytes.try_reserve_exact(1).map_err(|_| Failure::Bounds)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// Decode one complete record, including LF but excluding the transport EOF.
    /// The owner must prove EOF/process success independently.
    pub fn decode(bytes: &[u8], header: &Header, action: Action) -> Result<Self> {
        header.validate()?;
        if bytes.first() != Some(&b'{')
            || bytes.last() != Some(&b'\n')
            || bytes.get(bytes.len().saturating_sub(2)) != Some(&b'}')
            || bytes[..bytes.len().saturating_sub(1)]
                .iter()
                .any(|b| matches!(b, b'\r' | b'\n'))
            || bytes.len() > response_limit(action)
        {
            return Err(Failure::Framing);
        }
        let wire: WireResponse = decode(&bytes[..bytes.len() - 1], response_limit(action) - 1)?;
        if wire.schema_version != 1
            || wire.scope != "OpsCapacityResponse"
            || wire.action != action
            || wire.run != header.run
            || wire.revision != header.selection.revision
            || wire.artifact_id != header.artifact_id
            || wire.binary_sha256 != header.members.binary.sha256
        {
            return Err(Failure::Identity);
        }
        let value = wire.result;
        let invalid = || Failure::Framing;
        let result = match action {
            Action::Preflight => {
                ResultData::Preflight(serde_json::from_value(value).map_err(|_| invalid())?)
            }
            Action::Reserve => {
                ResultData::Reserve(serde_json::from_value(value).map_err(|_| invalid())?)
            }
            Action::Build => {
                ResultData::Build(serde_json::from_value(value).map_err(|_| invalid())?)
            }
            Action::Start => {
                ResultData::Start(serde_json::from_value(value).map_err(|_| invalid())?)
            }
            Action::Status => {
                ResultData::Status(serde_json::from_value(value).map_err(|_| invalid())?)
            }
            Action::Lookup => {
                ResultData::Lookup(serde_json::from_value(value).map_err(|_| invalid())?)
            }
            Action::Collect => {
                ResultData::Collect(serde_json::from_value(value).map_err(|_| invalid())?)
            }
            Action::Recover => {
                ResultData::Recover(serde_json::from_value(value).map_err(|_| invalid())?)
            }
            Action::Retire => {
                ResultData::Retire(serde_json::from_value(value).map_err(|_| invalid())?)
            }
            Action::Archive => return Err(Failure::Framing),
        };
        result.validate(header)?;
        Ok(Self { result })
    }

    /// Bind returned saved-workload data to the owner's original bytes, not E's pin.
    pub fn bind_workload(&self, header: &Header, original: &[u8]) -> Result<()> {
        header.validate()?;
        workload(original, header)?;
        self.result.validate(header)?;
        match &self.result {
            ResultData::Lookup(value) if value.request_json.as_bytes() != original => {
                Err(Failure::Identity)
            }
            ResultData::Collect(value) if value.request_json.as_bytes() != original => {
                Err(Failure::Identity)
            }
            ResultData::Retire(value) if value.request_sha256 != sha(original) => {
                Err(Failure::Identity)
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;

fn response_limit(action: Action) -> usize {
    if matches!(action, Action::Reserve | Action::Status) {
        MAX_STATUS
    } else {
        MAX_RESPONSE
    }
}
