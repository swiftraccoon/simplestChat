"""Validate narrowly scoped, owned, expiring security review exceptions.

An exception matches one scanner fingerprint and one exact scope string. There
are no glob, regex, severity, package-family or query-class waivers. Every record
must remain valid even when its scanner is not part of the current check, so an
expired review cannot be hidden by running only a convenient subset of checks.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from datetime import UTC, date, datetime
from pathlib import Path
from typing import TYPE_CHECKING, cast
from urllib.parse import urlsplit

from security_tools import bounded_file, record, require, string

if TYPE_CHECKING:
    from collections.abc import Sequence

DEFAULT_PATH = Path(__file__).resolve().parents[1] / "security/exceptions.json"
MAX_POLICY = 1024 * 1024
MAX_EXCEPTIONS = 256
SCANNERS = frozenset(
    {
        "zizmor",
        "gitleaks",
        "semgrep",
        "cargo-audit",
        "cargo-deny",
        "pip-audit",
        "npm-audit",
        "squawk",
        "grype",
        "image-license",
        "codeql",
    }
)
FIELDS = {
    "scanner",
    "fingerprint",
    "scope",
    "owner",
    "rationale",
    "reachability",
    "expires",
    "review",
}


@dataclass(frozen=True)
class ExceptionRecord:
    """A complete review bound to one finding and one application context."""

    scanner: str
    fingerprint: str
    scope: str
    owner: str
    rationale: str
    reachability: str
    expires: date
    review: str


def parse_exception(value: object, today: date) -> ExceptionRecord:
    """Reject incomplete, wildcarded, unowned or expired exception records."""
    raw = record(value, FIELDS)
    values = {key: string(item) for key, item in raw.items()}
    require(values["scanner"] in SCANNERS, "unknown_exception_scanner")
    for key in ("fingerprint", "scope", "owner"):
        require(
            values[key] == values[key].strip()
            and not any(character in values[key] for character in "*?\n\r\x00")
            and values[key].lower() not in {"all", "any", "unknown", "todo"},
            "ambiguous_exception_scope",
        )
    require(re.fullmatch(r"\d{4}-\d{2}-\d{2}", values["expires"]), "invalid_exception_expiry")
    expires = date.fromisoformat(values["expires"])
    require(expires >= today, "expired_security_exception")
    review = urlsplit(values["review"])
    require(
        review.scheme == "https"
        and bool(review.hostname)
        and review.username is None
        and review.password is None
        and review.path not in {"", "/"},
        "invalid_exception_review_link",
    )
    return ExceptionRecord(
        scanner=values["scanner"],
        fingerprint=values["fingerprint"],
        scope=values["scope"],
        owner=values["owner"],
        rationale=values["rationale"],
        reachability=values["reachability"],
        expires=expires,
        review=values["review"],
    )


def read_exceptions(
    path: Path = DEFAULT_PATH, *, today: date | None = None
) -> list[ExceptionRecord]:
    """Validate the complete policy; duplicate identities fail instead of shadowing."""
    raw = record(
        cast("object", json.loads(bounded_file(path, MAX_POLICY))),
        {"schemaVersion", "exceptions"},
    )
    require(
        type(raw["schemaVersion"]) is int and raw["schemaVersion"] == 1, "exception_policy_schema"
    )
    require(isinstance(raw["exceptions"], list), "invalid_exception_list")
    entries = cast("list[object]", raw["exceptions"])
    require(len(entries) <= MAX_EXCEPTIONS, "too_many_security_exceptions")
    current = today if today is not None else datetime.now(UTC).date()
    result = [parse_exception(entry, current) for entry in entries]
    identities = {(entry.scanner, entry.fingerprint, entry.scope) for entry in result}
    require(len(identities) == len(result), "duplicate_security_exception")
    return result


def permitted(
    records: Sequence[ExceptionRecord], scanner: str, fingerprint: str, scope: str
) -> bool:
    """Match exact reviewed identity; similar versions, files or findings are not waived."""
    return any(
        item.scanner == scanner and item.fingerprint == fingerprint and item.scope == scope
        for item in records
    )
