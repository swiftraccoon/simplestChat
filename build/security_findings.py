"""Enforce scanner findings, immutable migration review and dependency budgets.

Scanner exit zero alone is insufficient: SARIF/reporting tools can successfully
emit a report containing findings. Each supported report has an explicit parser
and exact review identity. Unknown schemas and scanner errors fail the gate.
"""

from __future__ import annotations

import hashlib
import json
import re
import tomllib
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import TYPE_CHECKING, cast

from security_policy import permitted
from security_tools import bounded_file, record, require, string

if TYPE_CHECKING:
    from collections.abc import Sequence

    from security_policy import ExceptionRecord

MAX_POLICY = 1024 * 1024


def object_value(value: object) -> dict[str, object]:
    """Require an object without assuming a scanner's optional fields are present."""
    require(isinstance(value, dict), "scanner_object_expected")
    return cast("dict[str, object]", value)


def list_value(value: object) -> list[object]:
    """Require a report list instead of interpreting missing data as zero findings."""
    require(isinstance(value, list), "scanner_list_expected")
    return cast("list[object]", value)


def digest(value: object) -> str:
    """Fingerprint reviewed configuration independent of JSON formatting."""
    return hashlib.sha256(
        json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


def gitleaks_configuration(path: Path, reviews: Sequence[ExceptionRecord]) -> None:
    """Every public fixture allowlist is exact, conjunctive and backed by a current review."""
    raw = record(
        cast("object", tomllib.loads(bounded_file(path, MAX_POLICY).decode())),
        {"title", "extend", "allowlists"},
    )
    require(raw["extend"] == {"useDefault": True}, "gitleaks_default_rules_required")
    for value in list_value(raw["allowlists"]):
        entry = record(value, {"description", "condition", "paths", "regexTarget", "regexes"})
        paths, patterns = list_value(entry["paths"]), list_value(entry["regexes"])
        require(
            entry["condition"] == "AND" and len(paths) == 1 and len(patterns) == 1,
            "gitleaks_broad_allowlist",
        )
        scope = string(entry["description"]).removeprefix("Public fixture: ")
        require(
            permitted(reviews, "gitleaks", "public-fixture:" + digest(entry), scope),
            "unreviewed_gitleaks_allowlist",
        )


def zizmor_findings(data: bytes, reviews: Sequence[ExceptionRecord]) -> int:
    """Bind each review to the exact audit, source feature and symbolic YAML location."""
    count = 0
    for value in list_value(cast("object", json.loads(data))):
        finding = object_value(value)
        require(not finding.get("ignored", False), "zizmor_inline_ignore_forbidden")
        locations = [object_value(item) for item in list_value(finding["locations"])]
        primary = [
            item for item in locations if object_value(item["symbolic"])["kind"] == "Primary"
        ]
        require(len(primary) == 1, "zizmor_primary_location")
        location = primary[0]
        symbolic = object_value(location["symbolic"])
        local = object_value(object_value(symbolic["key"])["Local"])
        route = list_value(object_value(symbolic["route"])["route"])
        parts: list[str] = []
        for item in route:
            component = object_value(item)
            require(len(component) == 1 and set(component) <= {"Key", "Index"}, "zizmor_route")
            parts.append(str(next(iter(component.values()))))
        scope = string(local["verbatim_path"]) + "#" + "/".join(parts)
        feature = string(object_value(location["concrete"])["feature"])
        fingerprint = string(finding["ident"]) + ":" + hashlib.sha256(feature.encode()).hexdigest()
        require(permitted(reviews, "zizmor", fingerprint, scope), "unreviewed_zizmor_finding")
        count += 1
    return count


def cargo_findings(data: bytes, reviews: Sequence[ExceptionRecord], dependencies: int) -> int:
    """Fail every unreviewed advisory, including warning-class dependency health issues."""
    raw = object_value(cast("object", json.loads(data)))
    require(
        raw["settings"]
        == {
            "target_arch": [],
            "target_os": [],
            "severity": None,
            "ignore": [],
            "informational_warnings": ["unmaintained", "unsound", "notice"],
        },
        "cargo_filtered_report",
    )
    lock = object_value(raw["lockfile"])
    require(
        type(lock["dependency-count"]) is int and lock["dependency-count"] == dependencies,
        "cargo_incomplete_lockfile",
    )
    database = object_value(raw["database"])
    require(
        re.fullmatch(r"[0-9a-f]{40}", string(database["last-commit"]))
        and type(database["advisory-count"]) is int
        and database["advisory-count"] > 0,
        "cargo_database_identity",
    )
    updated = datetime.fromisoformat(string(database["last-updated"]))
    require(
        updated.tzinfo is not None
        and timedelta(hours=-2) <= datetime.now(UTC) - updated <= timedelta(days=30),
        "cargo_database_stale",
    )
    vulnerabilities = object_value(raw["vulnerabilities"])
    entries = list(list_value(vulnerabilities["list"]))
    require(
        type(vulnerabilities["count"]) is int
        and vulnerabilities["count"] == len(entries)
        and type(vulnerabilities["found"]) is bool
        and vulnerabilities["found"] == bool(entries),
        "cargo_inconsistent_report",
    )
    for values in object_value(raw["warnings"]).values():
        entries.extend(list_value(values))
    for value in entries:
        item = object_value(value)
        package = object_value(item["package"])
        advisory = object_value(item["advisory"])
        scope = string(package["name"]) + "@" + string(package["version"])
        require(
            permitted(reviews, "cargo-audit", string(advisory["id"]), scope),
            "unreviewed_cargo_advisory",
        )
    return len(entries)


def dependency_budget(data: bytes, path: Path) -> None:
    """Require a reviewed budget change for new duplicate groups or versions."""
    metadata = object_value(cast("object", json.loads(data)))
    versions: dict[str, set[str]] = {}
    for value in list_value(metadata["packages"]):
        package = object_value(value)
        versions.setdefault(string(package["name"]), set()).add(string(package["version"]))
    raw = record(
        cast("object", json.loads(bounded_file(path, MAX_POLICY))),
        {"schemaVersion", "reviewedRevision", "duplicates"},
    )
    require(raw["schemaVersion"] == 1, "dependency_budget_schema")
    allowed = object_value(raw["duplicates"])
    for name, current in versions.items():
        if len(current) > 1:
            require(name in allowed, "new_duplicate_dependency_group")
            budget = {string(item) for item in list_value(allowed[name])}
            require(current <= budget, "new_duplicate_dependency_version")


def new_migrations(root: Path) -> list[str]:
    """Existing deployed SQL is immutable; all files beyond the initial review are linted."""
    path = root / "security/migration-baseline.json"
    raw = record(
        cast("object", json.loads(bounded_file(path, MAX_POLICY))),
        {"schemaVersion", "reviewedRevision", "files"},
    )
    require(raw["schemaVersion"] == 1, "migration_baseline_schema")
    baseline: dict[str, str] = {}
    for item in list_value(raw["files"]):
        entry = record(item, {"path", "sha256"})
        name = string(entry["path"])
        require(name not in baseline, "duplicate_reviewed_migration")
        baseline[name] = string(entry["sha256"])
    current = {str(path.relative_to(root)) for path in (root / "migrations").glob("*.sql")}
    require(set(baseline) <= current, "reviewed_migration_deleted")
    for name, expected in baseline.items():
        require(
            name.startswith("migrations/") and Path(name).name == name.removeprefix("migrations/"),
            "migration_baseline_path",
        )
        require(
            hashlib.sha256(bounded_file(root / name, MAX_POLICY)).hexdigest() == expected,
            "reviewed_migration_modified",
        )
    return sorted(current - set(baseline))
