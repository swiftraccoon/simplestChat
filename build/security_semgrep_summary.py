"""Keep failed Semgrep diagnostics useful without publishing scanner content."""

from __future__ import annotations

import re
from pathlib import Path

import security_semgrep as scanner
from release_json import JsonObject, JsonValue, array_value, decode_json, object_value
from security_tools import ToolError, bounded_file, require, safe_name

MAX_ITEMS = 32
MAX_ENGINE_REPORTS = 2
CORE_VARIANT_FIELDS = 2
MAX_METADATA_DEPTH = 4
MAX_METADATA_NODES = 64
MAX_REPORT = 16 * 1024**2
ERRORS = frozenset(
    {
        "semgrep_check_failed",
        "semgrep_engine_errors",
        "semgrep_engine_version",
        "semgrep_skipped_rules",
        "semgrep_fixpoint_timeout",
        "semgrep_incomplete_rules",
        "semgrep_incomplete_scan",
        "semgrep_unknown_finding",
        "semgrep_finding_line",
        "semgrep_inline_ignore",
        "semgrep_golden_false_positive",
        "semgrep_golden_mismatch",
        "scanner_command_failed_source-engine",
        "scanner_command_failed_golden-engine",
    }
)
ENGINE_KINDS = frozenset(
    {
        "Timeout",
        "ParseError",
        "LexicalError",
        "PartialParsing",
        "MatchingError",
        "OutOfMemory",
        "StackOverflow",
        "FatalError",
        "OtherParseError",
        "AstBuilderError",
        "RuleParseError",
        "PatternParseError",
        "InvalidRuleSchemaError",
    }
)
FIELDS = frozenset(
    {
        "schemaVersion",
        "includeVendor",
        "toolLockSha256",
        "engineVersion",
        "rulesSha256",
        "sourceManifestSha256",
        "goldenAssertions",
        "sourceFiles",
        "findings",
        "passed",
    }
)


def source_paths(directory: Path) -> set[str]:
    """Only paths in the private immutable scan inventory can be made public."""
    try:
        manifest = object_value(
            decode_json(
                bounded_file(
                    directory / "source-manifest.json",
                    4 * scanner.MAX_CONFIG,
                    private=True,
                )
            )
        )
        require(0 < len(manifest) <= scanner.MAX_FILES, "semgrep_invalid_result")
        return {
            name
            for name in manifest
            if safe_name(name)
            and Path(name).suffix in scanner.LANGUAGES
            and name.startswith(scanner.SOURCE_PREFIXES)
        }
    except (OSError, ValueError, KeyError, TypeError, ToolError):
        return set()


def error_kind(value: JsonValue) -> str:
    """Expose only the reviewed tag from the pinned core's payload variants."""
    if isinstance(value, list) and len(value) == CORE_VARIANT_FIELDS:
        value = value[0]
    return value if isinstance(value, str) and value in ENGINE_KINDS else "other"


def engine_reports(directory: Path) -> list[JsonObject]:
    """Read only the two bounded private standalone-core reports."""
    result: list[JsonObject] = []
    files = list(directory.glob("*-engine.stdout"))
    if len(files) > MAX_ENGINE_REPORTS:
        return result
    for path in sorted(files):
        if not re.fullmatch(r"[0-9]{2}-(?:golden|source)-engine\.stdout", path.name):
            continue
        try:
            result.append(object_value(decode_json(bounded_file(path, MAX_REPORT, private=True))))
        except (OSError, ValueError, KeyError, TypeError, ToolError):
            continue
    return result


def engine_errors(reports: list[JsonObject], paths: set[str]) -> list[JsonValue]:
    """Expose fixed error kinds and inventoried coordinates, never raw messages."""
    result: list[JsonValue] = []
    for raw in reports:
        errors = raw.get("errors", [])
        if not isinstance(errors, list):
            continue
        for error in errors:
            entry = error if isinstance(error, dict) else {}
            public: JsonObject = {"kind": error_kind(entry.get("error_type"))}
            location = entry.get("location")
            if isinstance(location, dict):
                name = location.get("path")
                if isinstance(name, str) and name in paths:
                    public["path"] = name
                    start = location.get("start")
                    line = start.get("line") if isinstance(start, dict) else None
                    if type(line) is int and 0 < line <= scanner.MAX_LINE:
                        public["line"] = line
            result.append(public)
            if len(result) > MAX_ITEMS:
                return result
    return result


def timeout_coordinates(value: JsonValue, paths: set[str], identifiers: set[str]) -> JsonObject:
    """Recognize exact known paths/rules inside bounded core timeout metadata."""
    pending = [(value, 0)]
    names: set[str] = set()
    rules: set[str] = set()
    for _ in range(MAX_METADATA_NODES):
        if not pending:
            break
        entry, depth = pending.pop()
        if isinstance(entry, str):
            if entry in paths:
                names.add(entry)
            if entry in identifiers:
                rules.add(entry)
        elif depth < MAX_METADATA_DEPTH:
            children = list(entry.values()) if isinstance(entry, dict) else entry
            if isinstance(children, list):
                pending.extend((child, depth + 1) for child in children[:MAX_METADATA_NODES])
    result: JsonObject = {}
    if len(names) == 1:
        result["path"] = next(iter(names))
    if len(rules) == 1:
        result["rule"] = next(iter(rules))
    return result


def fixpoint_timeouts(reports: list[JsonObject], paths: set[str]) -> list[JsonValue]:
    """Retain timeout coordinates without publishing function names or messages."""
    result: list[JsonValue] = []
    identifiers = scanner.rules(scanner.ROOT / scanner.RULES)
    for raw in reports:
        timing = raw.get("time")
        entries: JsonValue = timing.get("fixpoint_timeouts", []) if isinstance(timing, dict) else []
        if not isinstance(entries, list):
            continue
        for entry in entries[: MAX_ITEMS + 1]:
            result.append(timeout_coordinates(entry, paths, identifiers))
            if len(result) > MAX_ITEMS:
                return result
    return result


def findings(raw: JsonObject, paths: set[str]) -> list[JsonValue]:
    """Validate all coordinates before truncation so malformed tails fail closed."""
    identifiers = scanner.rules(scanner.ROOT / scanner.RULES)
    result: list[JsonValue] = []
    for item in array_value(raw["findings"]):
        entry = object_value(item)
        require(set(entry) == {"rule", "path", "line"}, "semgrep_invalid_result")
        rule, path, line = entry["rule"], entry["path"], entry["line"]
        require(
            isinstance(rule, str)
            and rule in identifiers
            and isinstance(path, str)
            and path in paths
            and type(line) is int
            and 0 < line <= scanner.MAX_LINE,
            "semgrep_invalid_result",
        )
        result.append(entry)
    return result


def diagnosis(status: int, data: bytes, directory: Path) -> JsonObject:
    """Parse only the nested safe report; every inconsistent or malformed result fails."""
    result: JsonObject = {"passed": False, "error": "semgrep_invalid_result"}
    try:
        require(len(data) <= MAX_REPORT, "semgrep_invalid_result")
        raw = object_value(decode_json(data))
        paths = source_paths(directory)
        if "error" in raw:
            require(
                set(raw) == {"passed", "error"} and raw["passed"] is False and status == 1,
                "semgrep_invalid_result",
            )
            code = raw["error"]
            result["error"] = (
                code if isinstance(code, str) and code in ERRORS else "semgrep_check_failed"
            )
            reports = engine_reports(directory)
            errors = engine_errors(reports, paths)
            result["engineErrors"] = errors[:MAX_ITEMS]
            result["engineErrorsTruncated"] = len(errors) > MAX_ITEMS
            timeouts = fixpoint_timeouts(reports, paths)
            result["fixpointTimeouts"] = timeouts[:MAX_ITEMS]
            result["fixpointTimeoutsTruncated"] = len(timeouts) > MAX_ITEMS
            return result
        require(
            set(raw) == set(FIELDS)
            and type(raw["schemaVersion"]) is int
            and raw["schemaVersion"] == 1
            and type(raw["passed"]) is bool
            and type(raw["includeVendor"]) is bool,
            "semgrep_invalid_result",
        )
        for key in ["sourceFiles", "goldenAssertions"]:
            value = raw[key]
            require(type(value) is int and 0 < value <= scanner.MAX_FILES, "semgrep_invalid_result")
        entries = findings(raw, paths)
        passed = not entries
        require(
            raw["passed"] is passed and status == (0 if passed else 1), "semgrep_invalid_result"
        )
        return {
            "passed": passed,
            "findingCount": len(entries),
            "findings": entries[:MAX_ITEMS],
            "findingsTruncated": len(entries) > MAX_ITEMS,
        }
    except (OSError, ValueError, KeyError, TypeError, ToolError):
        return result
