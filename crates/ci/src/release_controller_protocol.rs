//! Private capability records. No PID is stored or used as cross-step authority.

use super::plan::{Origin, Stage};
use serde::{Deserialize, Serialize};

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Binding {
    pub device: u64,
    pub inode: u64,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Intent {
    pub origin: Origin,
    pub capability: String,
    pub log: Binding,
    pub prefix: Binding,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Ready {
    pub intent_sha256: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Request {
    pub intent_sha256: String,
    pub capability: String,
    pub action: Action,
}
#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(super) enum Action {
    Commit,
    Cancel,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
/// Prepared certifies installer outcomes/current prefix, settled CLI groups and
/// closed log/status only. It never certifies worker exit or the subsequent
/// publisher-directory closes, which the collector cannot observe. Requirements
/// snapshot validation/close does precede terminal publication.
pub(super) struct Terminal {
    pub intent_sha256: String,
    pub installer_status: u8,
    pub interruption: Option<u8>,
    pub stage: Option<Stage>,
    pub cli_groups_settled: bool,
    pub log_closed: bool,
    pub log_sha256: String,
    pub log_bytes: u64,
    pub status_sha256: String,
    pub status_bytes: u64,
    pub prepared: bool,
}
