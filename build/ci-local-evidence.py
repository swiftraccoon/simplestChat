#!/usr/bin/env python3
"""Require executed local CI gates and every declared matrix entry, bound to one run."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
from pathlib import Path
from typing import cast

from security_codeql_tools import AUTOMATED_LANGUAGES
from security_findings import list_value, object_value
from security_tools import ToolError, bounded_file, require, string

ROOT = Path(__file__).resolve().parents[1]
MAX_DOCUMENT = 1024 * 1024


def document(path: Path) -> dict[str, object]:
    """Read a bounded, regular JSON object."""
    return object_value(cast("object", json.loads(bounded_file(path, MAX_DOCUMENT))))


def identity() -> dict[str, str]:
    """Bind each receipt to the launcher's nonce and exact candidate/base pair."""
    result = {
        "runId": os.environ["LOCAL_CI_RUN_ID"],
        "revision": os.environ["GITHUB_SHA"],
        "base": string(document(Path(os.environ["GITHUB_EVENT_PATH"]))["before"]),
    }
    require(re.fullmatch(r"[a-f0-9]{32}", result["runId"]) is not None, "local_ci_run_id")
    for field in ("revision", "base"):
        require(re.fullmatch(r"[a-f0-9]{40}", result[field]) is not None, "local_ci_revision")
    return result


def write_new(path: Path, value: dict[str, object]) -> None:
    """Publish one private receipt without replacing prior evidence."""
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    with path.open("x", encoding="utf-8") as target:
        _ = target.write(json.dumps(value, sort_keys=True, indent=2) + "\n")
    path.chmod(0o600)


def record(identifier: str) -> None:
    """Record only a successfully completed job's final step."""
    require(re.fullmatch(r"[a-z][a-z0-9-]{0,79}", identifier) is not None, "local_ci_check_id")
    output = Path(os.environ["LOCAL_CI_EVIDENCE"]) / "receipts" / f"{identifier}.json"
    write_new(output, {"schema": 1, "status": "passed", "check": identifier, **identity()})


def yaml_jobs(name: str) -> dict[str, object]:
    """Parse the canonical workflow with the existing checksum-pinned YAML dependency."""
    import yaml  # noqa: PLC0415 -- Recording receipts needs only the standard library.

    data = cast(
        "object", yaml.safe_load(bounded_file(ROOT / ".github/workflows" / name, MAX_DOCUMENT))
    )
    return object_value(object_value(data)["jobs"])


def matrix(job: object) -> dict[str, object]:
    """Reject missing or unrecognized matrix definitions."""
    return object_value(object_value(object_value(job)["strategy"])["matrix"])


def expected_checks() -> tuple[set[str], set[str]]:
    """Require the same first-party coverage locally and in hosted automation."""
    ci, security, codeql = yaml_jobs("ci.yml"), yaml_jobs("security.yml"), yaml_jobs("codeql.yml")
    required = {string(value) for value in list_value(object_value(ci["required"])["needs"])}
    browser = matrix(ci["browser"])
    require(set(browser) == {"group", "include"}, "local_ci_browser_matrix_shape")
    groups = [string(value) for value in list_value(browser["group"])]
    included = [string(object_value(value)["group"]) for value in list_value(browser["include"])]
    require(sorted(groups) == sorted(included), "local_ci_browser_matrix_ports")
    require("native-security" not in security, "local_ci_vendor_scan_is_optional")
    require("native-analysis" not in codeql, "local_ci_vendor_scan_is_optional")
    source = matrix(codeql["source-analysis"])
    require(set(source) == {"language"}, "local_ci_codeql_matrix_shape")
    languages = [string(value) for value in list_value(source["language"])]
    pinned = set(object_value(document(ROOT / "security/codeql-toolchain.json")["languages"]))
    require(
        set(languages) == set(AUTOMATED_LANGUAGES)
        and set(languages) <= pinned
        and len(languages) == len(AUTOMATED_LANGUAGES),
        "local_ci_codeql_coverage",
    )
    require(bool(groups) and len(groups) == len(set(groups)), "local_ci_browser_coverage")
    checks = {f"browser-{group}" for group in groups}
    checks |= {f"codeql-{language}" for language in languages} | {"security-fast"}
    return required, checks


def complete() -> None:
    """Require successful real jobs, every matrix receipt and an unchanged workflow inventory."""
    required, checks = expected_checks()
    needs = object_value(cast("object", json.loads(os.environ["GATE_RESULTS"])))
    require(set(needs) == required, "local_ci_gate_inventory")
    require(
        all(object_value(value).get("result") == "success" for value in needs.values()),
        "local_ci_gate_failure",
    )
    current = identity()
    evidence = Path(os.environ["LOCAL_CI_EVIDENCE"])
    observed = {path.stem for path in (evidence / "receipts").glob("*.json")}
    require(observed == checks, "local_ci_incomplete_matrix_receipts")
    for identifier in sorted(checks):
        receipt = document(evidence / "receipts" / f"{identifier}.json")
        expected: dict[str, object] = {
            "schema": 1,
            "status": "passed",
            "check": identifier,
            **current,
        }
        require(receipt == expected, "local_ci_receipt_identity")
    workflows = {
        name: hashlib.sha256(
            bounded_file(ROOT / ".github/workflows" / name, MAX_DOCUMENT)
        ).hexdigest()
        for name in ("ci.yml", "security.yml", "codeql.yml")
    }
    write_new(
        evidence / "required.json",
        {
            "schema": 1,
            "status": "passed",
            **current,
            "gates": sorted(required),
            "checks": sorted(checks),
            "workflows": workflows,
        },
    )


def requirements() -> None:
    """Reuse the controller's PyYAML version and wheel hashes without another lockfile."""
    source = bounded_file(ROOT / "ops/ansible/requirements.txt", MAX_DOCUMENT).decode()
    entries = [
        line for line in source.replace("\\\n", " ").splitlines() if line.startswith("pyyaml==")
    ]
    require(len(entries) == 1 and "--hash=sha256:" in entries[0], "local_ci_yaml_requirement")
    _ = sys.stdout.write(entries[0] + "\n")


def main() -> int:
    """Accept only the owned ACT runner's explicit receipt operations."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("operation", choices=("record", "complete", "requirements"))
    _ = parser.add_argument("--id", default="")
    options = parser.parse_args()
    try:
        require(
            os.environ.get("ACT") == "true" and os.environ.get("LOCAL_CI_DISPOSABLE") == "1",
            "local_ci_runner_only",
        )
        operation, identifier = cast("str", options.operation), cast("str", options.id)
        if operation == "record":
            record(identifier)
        elif operation == "complete":
            complete()
        else:
            requirements()
    except (ToolError, OSError, KeyError, ValueError) as error:
        _ = sys.stderr.write(f"Local CI evidence failed: {error}\n")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
