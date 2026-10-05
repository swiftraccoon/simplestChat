"""Keep vendor source diagnostics explicit and local while retaining build inputs."""

from __future__ import annotations

import os

from security_tools import require, safe_name

AUTOMATION_MARKERS = ("CI", "GITHUB_ACTIONS", "ACT")


def require_local_vendor(*, include_vendor: bool) -> None:
    """Refuse optional vendor scans in hosted or complete local CI execution."""
    require(
        not include_vendor or not any(name in os.environ for name in AUTOMATION_MARKERS),
        "vendor_source_scan_local_only",
    )


def vendor_path(name: str) -> bool:
    """Recognize only the canonical top-level vendored source tree."""
    return name.startswith("vendor/") and safe_name(name)


def selected(name: str, *, include_vendor: bool = False) -> bool:
    """Default diagnostics cover first-party paths; explicit local scans add vendor."""
    return include_vendor or not vendor_path(name)
