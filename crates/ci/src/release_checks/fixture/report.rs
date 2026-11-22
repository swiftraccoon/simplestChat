//! The public `report.json`: which phase the fixture reached, what failed by
//! fixed name, what each case proved and how cleanup ended. It holds no
//! command output, configuration, credential or database content.

use super::host::{Failed, Started};
use serde::Serialize;
use serde_json::Value;

/// The phase the fixture was in when it stopped, or `complete`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Preflight,
    Bootstrap,
    BrowserAssetRollover,
    StageCandidate,
    DeployCandidate,
    StageFailed,
    DeployFailed,
    Complete,
}

/// One app-only release case: the candidate's replacement, or the failed
/// candidate's rollback.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Release {
    pub passed: bool,
    pub release_passed: bool,
    pub rollback_passed: Value,
    pub dependencies_unchanged: bool,
    pub database_marker_preserved: bool,
    pub backup_verified: bool,
    pub stage_uninterrupted: bool,
    pub command_seconds: f64,
}

/// What each completed case proved.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Cases {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_response_policy: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_assets: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate: Option<Release>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<Release>,
}

/// The whole public report. `passed` is true only after every case and
/// cleanup passed and no signal or deadline interrupted the run.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Report {
    pub schema_version: u8,
    pub passed: bool,
    pub phase: Phase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    pub cases: Cases,
    pub cleanup_passed: bool,
    pub cleanup_failures: Vec<String>,
    pub execution: &'static str,
    pub reboot_tested: bool,
    pub derived_images_are_test_fixtures: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_asset_releases: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_asset_failure_stage: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_release: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validator_startup: Option<Value>,
    pub last_nonzero_command: Option<Failed>,
    pub last_command: Option<Started>,
    pub interrupted: bool,
    pub seconds: f64,
}
impl Report {
    pub(super) fn new() -> Self {
        Self {
            schema_version: 1,
            passed: false,
            phase: Phase::Preflight,
            failure: None,
            cases: Cases::default(),
            cleanup_passed: false,
            cleanup_failures: Vec::new(),
            execution: "real-containers",
            reboot_tested: false,
            derived_images_are_test_fixtures: true,
            base_image: None,
            source_revision: None,
            browser_asset_releases: None,
            browser_asset_failure_stage: None,
            last_release: None,
            validator_startup: None,
            last_nonzero_command: None,
            last_command: None,
            interrupted: false,
            seconds: 0.0,
        }
    }
}
