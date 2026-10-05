"""Run reviewed local rules with the authenticated standalone Semgrep engine.

Every check executes golden positive and negative fixtures with the actual engine
before scanning a bounded snapshot of maintained production sources. No registry
rules, Python CLI, network configuration or inline suppressions are consulted.
Full engine streams remain private; the public result contains only reviewed rule
identifiers, canonical relative paths and line numbers. A parser error, timeout,
missing target, unknown rule or unexpected fixture match fails the gate.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, cast

from security_context import Context
from security_source_scope import require_local_vendor, vendor_path
from security_tools import (
    ToolError,
    bounded_file,
    load_lock,
    require,
    safe_name,
    tool_path,
    write_private,
)

# isort: split
from release_json import JsonObject, JsonValue, array_value, decode_json, object_value, string_value

if TYPE_CHECKING:
    from collections.abc import Sequence

ROOT = Path(__file__).resolve().parents[1]
RULES = "security/semgrep/rules.json"
FIXTURES = "security/semgrep/fixtures"
MAX_CONFIG = 1024 * 1024
MAX_FILES = 20000
MAX_RULES = 100
MAX_LINE = 1000000
LANGUAGES = {".py": "python", ".rs": "rust", ".ts": "typescript", ".js": "typescript"}
SOURCE_PREFIXES = (
    "src/",
    "build/",
    "ops/ansible/files/",
    "ops/ansible/callback_plugins/",
    "ops/ansible/filter_plugins/",
    "web/src/",
)
RULE_ID = re.compile(r"simplestchat\.[a-z][a-z0-9-]{1,80}")
ANNOTATION = re.compile(r"^\s*(?:#|//)\s*(ruleid|ok):\s*(\S+)\s*$")
FORBIDDEN_RULE_KEYS = {
    "pattern-where-python",
    "fix",
    "fix-regex",
    "r2c-internal-project-depends-on",
}


def dump(path: Path, value: JsonValue) -> None:
    """Write one exclusive private JSON record with deterministic ordering."""
    write_private(path, (json.dumps(value, sort_keys=True, indent=2) + "\n").encode(), 0o600)


def safe_rule_tree(value: JsonValue) -> None:
    """Disallow executable predicates and mutation features in the local rule pack."""
    if isinstance(value, dict):
        require(not FORBIDDEN_RULE_KEYS.intersection(value), "semgrep_forbidden_rule_feature")
        for item in value.values():
            safe_rule_tree(item)
    elif isinstance(value, list):
        for item in value:
            safe_rule_tree(item)


def rules(path: Path) -> set[str]:
    """Require a nonempty, uniquely identified local pack with supported languages."""
    value = object_value(decode_json(bounded_file(path, MAX_CONFIG)))
    require(set(value) == {"rules"}, "semgrep_rules_schema")
    entries = array_value(value["rules"])
    require(0 < len(entries) <= MAX_RULES, "semgrep_rule_count")
    identifiers: set[str] = set()
    for entry in entries:
        rule = object_value(entry)
        identifier = string_value(rule["id"])
        require(RULE_ID.fullmatch(identifier), "semgrep_rule_id")
        require(identifier not in identifiers, "semgrep_duplicate_rule")
        require(rule.get("severity") == "ERROR", "semgrep_rule_severity")
        languages = array_value(rule["languages"])
        require(
            bool(languages) and all(item in LANGUAGES.values() for item in languages),
            "semgrep_rule_language",
        )
        safe_rule_tree(rule)
        identifiers.add(identifier)
    return identifiers


@dataclass(frozen=True, order=True)
class Finding:
    """Only stable non-content coordinates may leave the private scanner output."""

    rule: str
    path: str
    line: int

    def json(self) -> JsonObject:
        """Omit matched source, metavariables, messages and arbitrary engine strings."""
        return {"rule": self.rule, "path": self.path, "line": self.line}


def targets(paths: Sequence[str]) -> JsonValue:
    """Use the pinned core's explicit-target interface with repository-root paths."""
    result: list[JsonValue] = []
    require(0 < len(paths) <= MAX_FILES and len(set(paths)) == len(paths), "semgrep_target_count")
    for path in paths:
        require(safe_name(path) and Path(path).suffix in LANGUAGES, "semgrep_target_path")
        result.append(
            [
                "CodeTarget",
                {
                    "path": {"fpath": path, "ppath": "/" + path},
                    "analyzer": LANGUAGES[Path(path).suffix],
                    "products": ["sast"],
                },
            ]
        )
    return ["Targets", result]


def findings(
    data: bytes, paths: Sequence[str], identifiers: set[str], expected_version: str
) -> set[Finding]:
    """Require complete scanning and reject error output even when exit status is zero."""
    result = object_value(decode_json(data))
    require(result["version"] == expected_version, "semgrep_engine_version")
    require(not array_value(result["errors"]), "semgrep_engine_errors")
    require(not array_value(result["skipped_rules"]), "semgrep_skipped_rules")
    require(
        not array_value(object_value(result["time"])["fixpoint_timeouts"]),
        "semgrep_fixpoint_timeout",
    )
    engines = array_value(result["rules_by_engine"])
    require(
        len(engines) == len(identifiers)
        and {tuple(array_value(item)) for item in engines}
        == {(identifier, "OSS") for identifier in identifiers},
        "semgrep_incomplete_rules",
    )
    scanned = array_value(object_value(result["paths"])["scanned"])
    require(
        len(scanned) == len(paths) and {string_value(item) for item in scanned} == set(paths),
        "semgrep_incomplete_scan",
    )
    found: set[Finding] = set()
    for item in array_value(result["results"]):
        match = object_value(item)
        identifier, path = string_value(match["check_id"]), string_value(match["path"])
        require(identifier in identifiers and path in paths, "semgrep_unknown_finding")
        line = object_value(match["start"])["line"]
        require(type(line) is int and 0 < line <= MAX_LINE, "semgrep_finding_line")
        require(not object_value(match["extra"]).get("is_ignored"), "semgrep_inline_ignore")
        found.add(Finding(identifier, path, cast("int", line)))
    return found


def run_engine(  # noqa: PLR0913 -- The immutable engine, rules and source inventory are independent identities.
    context: Context,
    engine: Path,
    rule_path: Path,
    root: Path,
    paths: Sequence[str],
    *,
    name: str,
) -> set[Finding]:
    """Bound engine time, memory, concurrency and report bytes before parsing results."""
    target_path = context.output / (name + "-targets.json")
    dump(target_path, targets(paths))
    _, result = context.run(
        name,
        [
            str(engine),
            "-rules",
            str(rule_path),
            "-targets",
            str(target_path),
            "-json_nodots",
            "-no_filter_irrelevant_rules",
            "-no_filter_irrelevant_patterns",
            "-j",
            "2",
            "-timeout",
            "5",
            "-timeout_threshold",
            "1",
            "-max_memory",
            "2048",
            "-max_match_per_file",
            "1000",
            "-strict",
        ],
        cwd=root,
        timeout=180,
    )
    tools, _ = load_lock()
    return findings(result, paths, rules(rule_path), tools["semgrep-core"].version)


def fixture_expectations(root: Path, identifiers: set[str]) -> tuple[set[Finding], set[Finding]]:
    """Parse exact next-line annotations; every rule needs positive and negative coverage."""
    positive: set[Finding] = set()
    negative: set[Finding] = set()
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        name = path.relative_to(root).as_posix()
        require(safe_name(name) and path.suffix in LANGUAGES, "semgrep_fixture_path")
        lines = bounded_file(path, MAX_CONFIG).decode().splitlines()
        for index, line in enumerate(lines):
            annotation = ANNOTATION.fullmatch(line)
            if annotation is None:
                require("ruleid:" not in line and "ok:" not in line, "semgrep_fixture_annotation")
                continue
            kind, identifier = annotation.groups()
            require(identifier in identifiers, "semgrep_fixture_unknown_rule")
            require(index + 1 < len(lines), "semgrep_fixture_missing_target")
            require(
                bool(lines[index + 1].strip()) and ANNOTATION.fullmatch(lines[index + 1]) is None,
                "semgrep_fixture_missing_target",
            )
            entry = Finding(identifier, name, index + 2)
            (positive if kind == "ruleid" else negative).add(entry)
    require({item.rule for item in positive} == identifiers, "semgrep_positive_coverage")
    require({item.rule for item in negative} == identifiers, "semgrep_negative_coverage")
    return positive, negative


def golden(context: Context, engine: Path, snapshot: Path) -> int:
    """Execute fixtures as source files without importing or evaluating their contents."""
    destination = context.output / "golden"
    destination.mkdir(mode=0o700)
    for path in sorted((snapshot / FIXTURES).rglob("*")):
        if not path.is_file():
            continue
        name = path.relative_to(snapshot / FIXTURES).as_posix()
        require(name.endswith(".fixture"), "semgrep_fixture_extension")
        target = destination / name.removesuffix(".fixture")
        target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        write_private(target, bounded_file(path, MAX_CONFIG), 0o600)
    identifiers = rules(snapshot / RULES)
    positive, negative = fixture_expectations(destination, identifiers)
    paths = sorted(
        path.relative_to(destination).as_posix()
        for path in destination.rglob("*")
        if path.is_file()
    )
    actual = run_engine(context, engine, snapshot / RULES, destination, paths, name="golden-engine")
    dump(
        context.output / "golden.json",
        {
            "expected": [item.json() for item in sorted(positive)],
            "actual": [item.json() for item in sorted(actual)],
            "negative": [item.json() for item in sorted(negative)],
        },
    )
    require(not actual.intersection(negative), "semgrep_golden_false_positive")
    require(actual == positive, "semgrep_golden_mismatch")
    return len(positive) + len(negative)


def source_paths(snapshot: Path, *, include_vendor: bool = False) -> list[str]:
    """Scope this curated pack to maintained production Rust, Python and browser code."""
    paths: list[str] = []
    for path in snapshot.rglob("*"):
        name = path.relative_to(snapshot).as_posix()
        if (
            path.is_file()
            and path.suffix in LANGUAGES
            and (name.startswith(SOURCE_PREFIXES) or (include_vendor and vendor_path(name)))
        ):
            paths.append(name)
    require(bool(paths), "semgrep_empty_source_scope")
    return sorted(paths)


def new_output(root: Path, output: Path) -> Path:
    """Keep in-checkout evidence inside the two maintained ignored artifact roots."""
    require(not output.exists() and not output.is_symlink(), "semgrep_output_exists")
    output = output.parent.resolve() / output.name
    require(
        not output.is_relative_to(root)
        or any(output.is_relative_to(root / name) for name in ("target", "results")),
        "semgrep_output_in_source",
    )
    output.mkdir(mode=0o700)
    return output


def check(
    root: Path, output: Path, tools_directory: Path | None = None, *, include_vendor: bool = False
) -> JsonObject:
    """Verify the engine, snapshot sources, run golden tests and scan the same bytes."""
    require_local_vendor(include_vendor=include_vendor)
    root = root.resolve()
    output = new_output(root, output)
    context = Context(root, output)
    tools, lock_digest = load_lock()
    engine = tool_path("semgrep-core", tools_directory)
    snapshot = context.snapshot()
    count = golden(context, engine, snapshot)
    paths = source_paths(snapshot, include_vendor=include_vendor)
    actual = run_engine(context, engine, snapshot / RULES, snapshot, paths, name="source-engine")
    require(load_lock()[1] == lock_digest, "semgrep_tool_lock_changed")
    report: JsonObject = {
        "schemaVersion": 1,
        "includeVendor": include_vendor,
        "toolLockSha256": lock_digest,
        "engineVersion": tools["semgrep-core"].version,
        "rulesSha256": hashlib.sha256(bounded_file(snapshot / RULES, MAX_CONFIG)).hexdigest(),
        "sourceManifestSha256": hashlib.sha256(
            bounded_file(output / "source-manifest.json", 4 * MAX_CONFIG)
        ).hexdigest(),
        "goldenAssertions": count,
        "sourceFiles": len(paths),
        "findings": [item.json() for item in sorted(actual)],
        "passed": not actual,
    }
    dump(output / "report.json", report)
    return report


class Arguments(argparse.Namespace):
    """Explicit parser result types keep command boundaries statically checked."""

    root: Path = ROOT
    output: Path = Path()
    tools_directory: Path | None = None
    include_vendor: bool = False


def main(argv: Sequence[str] | None = None) -> int:
    """Expose one fixed local check command and redact all unexpected diagnostics."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("command", choices=["check"])
    _ = parser.add_argument("--root", type=Path, default=ROOT)
    _ = parser.add_argument("--output", type=Path, required=True)
    _ = parser.add_argument("--tools-directory", type=Path)
    _ = parser.add_argument("--include-vendor", action="store_true")
    arguments = parser.parse_args(argv, namespace=Arguments())
    try:
        result = check(
            arguments.root,
            arguments.output,
            arguments.tools_directory,
            include_vendor=arguments.include_vendor,
        )
    except ToolError as error:
        _ = sys.stdout.write(json.dumps({"passed": False, "error": str(error)}) + "\n")
        return 1
    except (OSError, ValueError, KeyError, TypeError, RuntimeError):
        _ = sys.stdout.write(json.dumps({"passed": False, "error": "semgrep_check_failed"}) + "\n")
        return 1
    _ = sys.stdout.write(json.dumps(result, sort_keys=True) + "\n")
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
