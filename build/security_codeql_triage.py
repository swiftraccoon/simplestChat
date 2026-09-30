"""Enforce exact CodeQL reviews and separately plan authorized alert dismissals.

SARIF checks are local and need no GitHub token. API plan/check are read-only;
apply requires an explicit flag, an unchanged plan and fresh exact alert reads.
No query, path, severity class or remote dismissal acts as an exclusion.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import sys
import time
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import TYPE_CHECKING
from urllib.parse import unquote, urlencode

import security_policy
from security_tools import ToolError, bounded_file, require, write_private

# isort: split
import bounded_process
from release_json import JsonObject, JsonValue, array_value, decode_json, object_value, string_value

if TYPE_CHECKING:
    from collections.abc import Callable, Sequence

    from security_policy import ExceptionRecord

ROOT = Path(__file__).resolve().parents[1]
MAX_REPORT = 64 * 1024**2
MAX_SOURCE = 4 * 1024**2
MAX_PAGE = 100
MAX_PAGES = 20
MAX_APPLY = 64
MAX_SECONDS = 300
MAX_FINDINGS = 2000
HIGH_SCORE = 7
CRITICAL_SCORE = 9
MAX_SCORE = 10
MEDIUM_SCORE = 4
LANGUAGES = frozenset({"actions", "javascript-typescript", "python", "rust", "c-cpp"})
SEVERITIES = frozenset({"critical", "high", "medium", "low", "advisory"})
REGION = ("start_line", "end_line", "start_column", "end_column")
REVIEW_PATH = ROOT / "security/codeql-review-2026-09-30.json"


def digest(value: JsonValue) -> str:
    """Hash a complete structured identity independent of object-key formatting."""
    return hashlib.sha256(
        json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


def positive(value: JsonValue) -> int:
    """Reject booleans, missing locations and nonpositive identifiers."""
    require(type(value) is int and value > 0, "codeql_positive_integer")
    return int(str(value))


def source_hash(root: Path, revision: str, name: str) -> str:
    """Bind findings to the complete tracked file and reject path escapes or edited source."""
    path = PurePosixPath(name)
    require(
        name == str(path)
        and not path.is_absolute()
        and bool(path.parts)
        and not any(part in {".", ".."} for part in path.parts)
        and not any(character in name for character in "\\\n\r\x00"),
        "codeql_source_path",
    )
    require(re.fullmatch(r"[a-f0-9]{40}", revision), "codeql_revision")
    selected = root / name
    require(selected.resolve() == selected.absolute(), "codeql_source_symlink")
    data = bounded_file(selected, MAX_SOURCE)
    status, original, _ = bounded_process.run(
        ["git", "show", f"{revision}:{name}"],
        cwd=root,
        limits=bounded_process.Limits(timeout=20, stdout=MAX_SOURCE, stderr=65536),
    )
    require(status == 0 and data == original, "codeql_source_revision_mismatch")
    return hashlib.sha256(data).hexdigest()


def finding(  # noqa: PLR0913 -- Independent source, query and location evidence.
    root: Path, revision: str, rule: str, version: str, location: JsonObject, *, message: str
) -> JsonObject:
    """Use the same complete primary source/range identity for SARIF and API findings."""
    name = string_value(location["path"])
    region: JsonObject = {key: positive(location[key]) for key in REGION}
    require(positive(region["end_line"]) >= positive(region["start_line"]), "codeql_region_order")
    # GitHub renders SARIF's numbered related-location links as plain labels.
    # No other Markdown or message normalization is performed.
    rendered = re.sub(r"\[([^\[\]\n]+)\]\([0-9]+\)", r"\1", message)
    identity: JsonObject = {
        "rule": rule,
        "toolVersion": version,
        "path": name,
        "region": region,
        "messageSha256": hashlib.sha256(rendered.encode()).hexdigest(),
        "sourceSha256": source_hash(root, revision, name),
    }
    scope = name + ":" + ":".join(str(region[key]) for key in REGION)
    return {**identity, "scope": scope, "fingerprint": "codeql:" + digest(identity)}


def verdict(findings: list[JsonObject], reviews: Sequence[ExceptionRecord]) -> JsonObject:
    """Retain lower-severity findings and enforce every unreviewed High/Critical result."""
    blocked: list[JsonValue] = []
    waived: list[JsonValue] = []
    advisory: list[JsonValue] = []
    for item in findings:
        severity = string_value(item["severity"])
        require(severity in SEVERITIES, "codeql_unknown_severity")
        reviewed = security_policy.permitted(
            reviews, "codeql", string_value(item["fingerprint"]), string_value(item["scope"])
        )
        selected = waived if reviewed else blocked if severity in {"high", "critical"} else advisory
        selected.append(item)
    return {"passed": not blocked, "blocked": blocked, "waived": waived, "advisory": advisory}


def sarif_score(rule: JsonObject) -> str:
    """Security severity comes from the query, never SARIF's presentation level."""
    properties = object_value(rule.get("properties", {}))
    score = properties.get("security-severity")
    if score is None:
        require("security" not in array_value(properties.get("tags", [])), "codeql_missing_score")
        return "advisory"
    text = string_value(score)
    require(re.fullmatch(r"(?:[0-9](?:\.[0-9]+)?|10(?:\.0+)?)", text), "codeql_invalid_score")
    value = float(text)
    require(0 <= value <= MAX_SCORE, "codeql_invalid_score")
    if value >= CRITICAL_SCORE:
        return "critical"
    if value >= HIGH_SCORE:
        return "high"
    return "medium" if value >= MEDIUM_SCORE else "low"


def sarif_findings(raw: JsonObject, root: Path, revision: str) -> list[JsonObject]:
    """Require successful nonempty analysis metadata and reject suppressed/unknown report shapes."""
    require(raw.get("version") == "2.1.0", "codeql_sarif_version")
    runs = array_value(raw["runs"])
    require(bool(runs) and len(runs) <= len(LANGUAGES), "codeql_sarif_runs")
    result: list[JsonObject] = []
    deadline = time.monotonic() + MAX_SECONDS
    for value in runs:
        run = object_value(value)
        tool = object_value(run["tool"])
        driver = object_value(tool["driver"])
        require(driver["name"] == "CodeQL", "codeql_sarif_tool")
        version = string_value(driver.get("semanticVersion", driver.get("version")))
        invocations = array_value(run["invocations"])
        require(bool(invocations), "codeql_missing_execution")
        for invocation in invocations:
            details = object_value(invocation)
            require(details.get("executionSuccessful") is True, "codeql_execution_failed")
            for key in ("toolExecutionNotifications", "toolConfigurationNotifications"):
                require(
                    all(
                        object_value(notice).get("level") not in {"error", "warning"}
                        for notice in array_value(details.get(key, []))
                    ),
                    "codeql_incomplete_execution",
                )
        rules: dict[str, JsonObject] = {}
        for component in [
            driver,
            *[object_value(x) for x in array_value(tool.get("extensions", []))],
        ]:
            for item in array_value(component.get("rules", [])):
                rule = object_value(item)
                identifier = string_value(rule["id"])
                require(identifier not in rules, "codeql_duplicate_rule")
                rules[identifier] = rule
        require(bool(rules), "codeql_missing_rules")
        for entry in array_value(run["results"]):
            require(
                len(result) < MAX_FINDINGS and time.monotonic() < deadline, "codeql_report_limit"
            )
            item = object_value(entry)
            require(
                not item.get("suppressions") and item.get("kind", "fail") == "fail",
                "codeql_filtered_result",
            )
            require(item.get("baselineState") != "absent", "codeql_filtered_result")
            identifier = string_value(item["ruleId"])
            require(identifier in rules, "codeql_unknown_rule")
            locations = array_value(item["locations"])
            require(len(locations) == 1, "codeql_primary_location")
            physical = object_value(object_value(locations[0])["physicalLocation"])
            artifact = object_value(physical["artifactLocation"])
            region = object_value(physical["region"])
            location: JsonObject = {
                "path": unquote(string_value(artifact["uri"])),
                "start_line": region["startLine"],
                "end_line": region["endLine"],
                "start_column": region["startColumn"],
                "end_column": region["endColumn"],
            }
            result.append(
                {
                    **finding(
                        root,
                        revision,
                        identifier,
                        version,
                        location,
                        message=string_value(object_value(item["message"])["text"]),
                    ),
                    "severity": sarif_score(rules[identifier]),
                }
            )
    return result


class Github:
    """Bound every API response, page count and complete operation to GitHub.com."""

    def __init__(self, repository: str) -> None:
        """Only an explicit validated owner/repository can select the target."""
        require(re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository), "codeql_repository")
        self.repository: str = repository
        self.deadline: float = time.monotonic() + MAX_SECONDS
        executable = shutil.which("gh")
        require(executable is not None, "codeql_gh_missing")
        self.executable: str = str(executable)

    def request(self, suffix: str, body: JsonObject | None = None) -> JsonValue:
        """No shell, external hostname, API retries or unbounded output is permitted."""
        remaining = self.deadline - time.monotonic()
        require(remaining > 0, "codeql_api_deadline")
        argv = [
            self.executable,
            "api",
            "--hostname",
            "github.com",
            "--method",
            "PATCH" if body else "GET",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "X-GitHub-Api-Version: 2022-11-28",
            f"repos/{self.repository}/code-scanning/{suffix}",
        ]
        if body:
            argv += ["--input", "-"]
        env = {key: value for key, value in os.environ.items() if key != "GH_DEBUG"}
        env.update({"GH_PROMPT_DISABLED": "1", "GH_HOST": "github.com", "GH_PAGER": "cat"})
        status, output, _ = bounded_process.run(
            argv,
            env=env,
            input_data=json.dumps(body).encode() if body else b"",
            limits=bounded_process.Limits(
                timeout=min(20, remaining), stdout=MAX_SOURCE, stderr=65536
            ),
        )
        require(status == 0, "codeql_api_failed")
        return decode_json(output)

    def pages(self, resource: str, reference: str) -> list[JsonObject]:
        """Request all states; remote dismissals are evidence, never policy authority."""
        entries: list[JsonObject] = []
        for page in range(1, MAX_PAGES + 1):
            query = urlencode(
                {"ref": reference, "tool_name": "CodeQL", "per_page": MAX_PAGE, "page": page}
            )
            selected = array_value(self.request(resource + "?" + query))
            require(len(selected) <= MAX_PAGE, "codeql_page_size")
            entries.extend(object_value(item) for item in selected)
            if len(selected) < MAX_PAGE:
                return entries
        require(len(entries) < MAX_PAGES * MAX_PAGE, "codeql_pagination_limit")
        return entries


def api_finding(raw: JsonObject, root: Path, revision: str, reference: str) -> JsonObject:
    """Validate the exact current reference; fixed historical alerts are handled separately."""
    tool = object_value(raw["tool"])
    require(tool["name"] == "CodeQL", "codeql_api_tool")
    instance = object_value(raw["most_recent_instance"])
    require(
        instance["ref"] == reference and instance["commit_sha"] == revision, "codeql_stale_alert"
    )
    require(instance["state"] in {"open", "dismissed"}, "codeql_instance_state")
    rule = object_value(raw["rule"])
    severity = rule.get("security_severity_level")
    if severity is None:
        require("security" not in array_value(rule.get("tags", [])), "codeql_missing_score")
        severity = "advisory"
    require(severity in SEVERITIES, "codeql_unknown_severity")
    return {
        **finding(
            root,
            revision,
            string_value(rule["id"]),
            string_value(tool["version"]),
            object_value(instance["location"]),
            message=string_value(object_value(instance["message"])["text"]),
        ),
        "number": positive(raw["number"]),
        "severity": severity,
        "state": raw["state"],
    }


def api_plan(
    client: Github, root: Path, revision: str, reference: str, reviews: Sequence[ExceptionRecord]
) -> JsonObject:
    """Require all five exact-revision analyses and assess every still-active alert."""
    require(
        re.fullmatch(r"refs/(?:heads/[A-Za-z0-9._/-]+|pull/[0-9]+/merge)", reference), "codeql_ref"
    )
    analyses = client.pages("analyses", reference)
    categories: set[str] = set()
    for item in analyses:
        if item["commit_sha"] != revision:
            continue
        require(
            item["ref"] == reference and object_value(item["tool"])["name"] == "CodeQL",
            "codeql_analysis_identity",
        )
        require(item["error"] == "" and item.get("warning", "") == "", "codeql_analysis_failed")
        require(positive(item["rules_count"]) > 0, "codeql_analysis_empty")
        categories.add(string_value(item["category"]).rstrip("/"))
    expected = {f"/language:{language}/security" for language in LANGUAGES}
    require(categories >= expected, "codeql_missing_current_analysis")
    findings: list[JsonObject] = []
    seen: set[int] = set()
    for raw in client.pages("alerts", reference):
        require(time.monotonic() < client.deadline, "codeql_api_deadline")
        number = positive(raw["number"])
        require(number not in seen, "codeql_duplicate_alert")
        seen.add(number)
        require(raw["state"] in {"open", "dismissed", "fixed"}, "codeql_alert_state")
        if object_value(raw["most_recent_instance"])["state"] == "fixed":
            continue
        findings.append(api_finding(raw, root, revision, reference))
    return {
        "schemaVersion": 1,
        "repository": client.repository,
        "revision": revision,
        "ref": reference,
        "analysisCategories": list[JsonValue](sorted(categories)),
        **verdict(findings, reviews),
        "policySha256": hashlib.sha256(
            bounded_file(security_policy.DEFAULT_PATH, security_policy.MAX_POLICY)
        ).hexdigest(),
    }


def false_positive(item: JsonObject, repository: str) -> bool:
    """Risk acceptances and unrelated repositories cannot become false-positive dismissals."""
    raw = object_value(decode_json(bounded_file(REVIEW_PATH, security_policy.MAX_POLICY)))
    require(raw["schemaVersion"] == 1, "codeql_review_schema")
    return raw["repository"] == repository and any(
        entry.get("disposition") == "false-positive"
        and entry.get("number") == item.get("number")
        and entry.get("fingerprint") == item.get("fingerprint")
        and entry.get("scope") == item.get("scope")
        for entry in [object_value(value) for value in array_value(raw["alerts"])]
    )


def apply_plan(
    client: Github,
    root: Path,
    plan: JsonObject,
    reviews: Sequence[ExceptionRecord],
    journal: Callable[[JsonObject], None],
) -> JsonObject:
    """Revalidate the plan and each exact reviewed alert immediately before PATCH."""
    require(plan["repository"] == client.repository, "codeql_apply_repository")
    revision, reference = string_value(plan["revision"]), string_value(plan["ref"])
    fresh = api_plan(client, root, revision, reference, reviews)
    require(fresh == plan, "codeql_apply_plan_changed")
    selected = [
        object_value(item)
        for item in array_value(plan["waived"])
        if object_value(item)["state"] == "open"
    ]
    require(len(selected) <= MAX_APPLY, "codeql_apply_limit")
    require(
        all(false_positive(item, client.repository) for item in selected),
        "codeql_not_false_positive",
    )
    completed: list[JsonValue] = []
    for expected in selected:
        number = positive(expected["number"])
        current = object_value(client.request(f"alerts/{number}"))
        require(
            api_finding(current, root, revision, reference) == expected,
            "codeql_apply_alert_changed",
        )
        comment = (
            "Reviewed false positive; "
            + string_value(expected["fingerprint"])
            + "; policy "
            + string_value(plan["policySha256"])
        )
        journal({"phase": "dismissal_requested", "number": number, "dismissed": completed.copy()})
        result = object_value(
            client.request(
                f"alerts/{number}",
                {
                    "state": "dismissed",
                    "dismissed_reason": "false positive",
                    "dismissed_comment": comment,
                },
            )
        )
        require(
            result["number"] == number
            and result["state"] == "dismissed"
            and result["dismissed_reason"] == "false positive",
            "codeql_dismissal_unconfirmed",
        )
        completed.append(number)
        journal({"phase": "dismissal_confirmed", "number": number, "dismissed": completed.copy()})
    return {
        "schemaVersion": 1,
        "repository": client.repository,
        "revision": revision,
        "dismissed": completed,
    }


@dataclass
class Options(argparse.Namespace):
    """Select explicit source/report identities; mutation requires a separate intent flag."""

    mode: str = ""
    input: Path | None = None
    output: Path = Path()
    repository: str = "swiftraccoon/simplestChat"
    revision: str = ""
    reference: str = "refs/heads/main"
    authorize_dismissals: bool = False


def main(argv: Sequence[str] | None = None) -> int:
    """Publish bounded evidence and fail High/Critical checks without modifying GitHub state."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("mode", choices=("sarif", "plan", "check", "apply"))
    _ = parser.add_argument("--input", type=Path)
    _ = parser.add_argument("--output", type=Path, required=True)
    _ = parser.add_argument("--repository", default="swiftraccoon/simplestChat")
    _ = parser.add_argument("--revision", required=True)
    _ = parser.add_argument("--ref", dest="reference", default="refs/heads/main")
    _ = parser.add_argument("--authorize-dismissals", action="store_true")
    args = parser.parse_args(argv, namespace=Options())
    created = False
    action_number = 0

    def journal(value: JsonObject) -> None:
        """Retain each requested/confirmed mutation before the next operation starts."""
        nonlocal action_number
        action_number += 1
        write_private(
            args.output / f"action-{action_number:03d}.json",
            (json.dumps(value, indent=2) + "\n").encode(),
            0o600,
        )

    try:
        require(re.fullmatch(r"[a-f0-9]{40}", args.revision), "codeql_revision")
        require(args.authorize_dismissals == (args.mode == "apply"), "codeql_apply_authorization")
        args.output.mkdir(mode=0o700)
        created = True
        write_private(
            args.output / "request.json",
            (
                json.dumps(
                    {
                        "mode": args.mode,
                        "repository": args.repository,
                        "revision": args.revision,
                        "ref": args.reference,
                    }
                )
                + "\n"
            ).encode(),
            0o600,
        )
        reviews = security_policy.read_exceptions()
        if args.mode in {"sarif", "apply"}:
            require(args.input is not None, "codeql_input_required")
            raw = object_value(decode_json(bounded_file(Path(str(args.input)), MAX_REPORT)))
            if args.mode == "sarif":
                output = verdict(sarif_findings(raw, ROOT, args.revision), reviews)
            else:
                require(
                    raw["revision"] == args.revision and raw["ref"] == args.reference,
                    "codeql_apply_selection",
                )
                output = apply_plan(Github(args.repository), ROOT, raw, reviews, journal)
        else:
            require(args.input is None, "codeql_unexpected_input")
            output = api_plan(Github(args.repository), ROOT, args.revision, args.reference, reviews)
        write_private(
            args.output / "report.json", (json.dumps(output, indent=2) + "\n").encode(), 0o600
        )
        return int(args.mode in {"sarif", "check"} and output.get("passed") is not True)
    except (ToolError, bounded_process.ProcessError, OSError, ValueError, KeyError, TypeError):
        if created:
            write_private(
                args.output / "failure.json",
                b'{"passed":false,"reason":"validation_or_operation_failed"}\n',
                0o600,
            )
        _ = sys.stderr.write(
            "CodeQL evidence or review validation failed; no unreviewed alert was dismissed.\n"
        )
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
