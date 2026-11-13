//! Which sources a diagnostic covers. First-party paths are always covered;
//! the vendored tree only in an explicit scan on a developer's own machine.

use crate::{Result, record::safe_name, require};

/// Variables whose presence means hosted or complete local CI.
pub const AUTOMATION_MARKERS: [&str; 3] = ["CI", "GITHUB_ACTIONS", "ACT"];

/// Whether any automation marker is `present`, whatever its value.
pub fn automated(present: impl Fn(&str) -> bool) -> bool {
    AUTOMATION_MARKERS.into_iter().any(present)
}

/// Refuse the optional vendor scan where automation runs.
pub fn require_local_vendor(include_vendor: bool, automated: bool) -> Result<()> {
    require(
        !include_vendor || !automated,
        "vendor_source_scan_local_only",
    )
}

/// Only a canonical path below the top-level vendored tree.
pub fn vendor_path(name: &str) -> bool {
    name.starts_with("vendor/") && safe_name(name)
}

/// Whether a diagnostic of `name` is in scope.
pub fn selected(name: &str, include_vendor: bool) -> bool {
    include_vendor || !vendor_path(name)
}
