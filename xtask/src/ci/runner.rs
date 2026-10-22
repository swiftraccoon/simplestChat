//! Explicit privileged CI context, independent of a particular image/install plan.

use crate::tooling::Environment;
use std::ffi::OsStr;

pub(super) fn text<'a>(environment: &'a Environment, key: &str) -> &'a str {
    environment
        .get(OsStr::new(key))
        .and_then(|value| value.to_str())
        .unwrap_or("")
}

pub(super) fn authorized(environment: &Environment, system: &str) -> bool {
    system == "Linux"
        && if text(environment, "ACT") == "true" {
            text(environment, "LOCAL_CI_DISPOSABLE") == "1"
        } else {
            text(environment, "GITHUB_ACTIONS") == "true"
                && text(environment, "RUNNER_ENVIRONMENT") == "github-hosted"
        }
}
