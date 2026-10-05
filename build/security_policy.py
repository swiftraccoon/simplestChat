"""Validate narrowly scoped, owned, expiring security review exceptions.

An exception matches one scanner fingerprint and one exact scope string. There
are no glob, regex, severity, package-family or query-class waivers. Every record
in the selected source scope must remain valid even when its scanner is not part
of the current check. Optional vendor source reviews are enforced only when that
source scope is explicitly selected; dependency and image reviews always apply.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from datetime import UTC, date, datetime
from pathlib import Path
from typing import TYPE_CHECKING, cast
from urllib.parse import parse_qsl, unquote, urlsplit

from security_source_scope import vendor_path
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


def qualified_package_scope(value: str, scanner: str) -> bool:
    """Permit a literal package-URL query delimiter, never a wildcard or ambiguous qualifier."""
    if scanner not in {"image-license", "grype"}:
        return False
    if not re.fullmatch(
        r"pkg:[a-z][a-z0-9.+-]*/[A-Za-z0-9._~%/+-]+@[A-Za-z0-9._~%+:-]+\?[^?#\s]+",
        value,
    ) or re.search(r"%(?![a-fA-F0-9]{2})", value):
        return False
    parsed = urlsplit(value)
    decoded = unquote(parsed.path)
    if any(character in decoded for character in "*?\n\r\x00") or any(
        component in {".", ".."} for component in decoded.split("/")
    ):
        return False
    try:
        pairs = parse_qsl(parsed.query, keep_blank_values=True, strict_parsing=True)
    except ValueError:
        return False
    return (
        bool(pairs)
        and len(pairs) == len({key for key, _ in pairs})
        and all(
            re.fullmatch(r"[a-z][a-z0-9_]*", key)
            and item
            and not any(character in item for character in "*?\n\r\x00")
            and item == item.strip()
            for key, item in pairs
        )
    )


def parse_exception(value: object, today: date) -> ExceptionRecord:
    """Reject incomplete, wildcarded, unowned or expired exception records."""
    raw = record(value, FIELDS)
    values = {key: string(item) for key, item in raw.items()}
    require(values["scanner"] in SCANNERS, "unknown_exception_scanner")
    for key in ("fingerprint", "scope", "owner"):
        require(
            values[key] == values[key].strip()
            and not any(character in values[key] for character in "*\n\r\x00")
            and (
                "?" not in values[key]
                or (key == "scope" and qualified_package_scope(values[key], values["scanner"]))
            )
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
    path: Path = DEFAULT_PATH, *, today: date | None = None, include_vendor: bool = False
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
    result: list[ExceptionRecord] = []
    validated: list[ExceptionRecord] = []
    for entry in entries:
        fields = record(entry, FIELDS)
        vendor_source = string(fields["scanner"]) in {
            "codeql",
            "gitleaks",
            "semgrep",
            "zizmor",
        } and (vendor_path(string(fields["scope"])))
        parsed = parse_exception(
            entry, date.min if vendor_source and not include_vendor else current
        )
        validated.append(parsed)
        if include_vendor or not vendor_source:
            result.append(parsed)
    identities = {(entry.scanner, entry.fingerprint, entry.scope) for entry in validated}
    require(len(identities) == len(validated), "duplicate_security_exception")
    return result


def permitted(
    records: Sequence[ExceptionRecord], scanner: str, fingerprint: str, scope: str
) -> bool:
    """Match exact reviewed identity; similar versions, files or findings are not waived."""
    return any(
        item.scanner == scanner and item.fingerprint == fingerprint and item.scope == scope
        for item in records
    )
