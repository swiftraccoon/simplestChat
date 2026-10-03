"""Reconcile main history protection after exact-head release gates pass.

Plan/check are read-only. Apply requires a clean committed checkout, the same
remote main revision, a successful trusted CI run and completed CodeQL analysis.
Only the named history ruleset is managed; unrelated repository rules are retained.
There is no operation that disables protection or deletes a ruleset.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

from security_context import executable
from security_tools import ToolError, bounded_file, require

# isort: split
import bounded_process
from release_json import JsonObject, JsonValue, array_value, decode_json, object_value, string_value

if TYPE_CHECKING:
    from collections.abc import Sequence

ROOT = Path(__file__).resolve().parents[1]
POLICY = ROOT / "security/rulesets.json"
FIELDS = {"name", "target", "enforcement", "bypass_actors", "conditions", "rules"}
GATE = "Required security and correctness checks"
GITHUB_ACTIONS = 15368
NAMES = {"Main history protection"}
OBSOLETE_NAMES = {"Main security gates", "Main pull request review"}
LANGUAGES = {"actions", "javascript-typescript", "python", "rust", "c-cpp"}
PAGE_LIMIT = 100


def command(argv: Sequence[str], data: bytes = b"") -> bytes:
    """Bound administrative command output and never echo credential-bearing errors."""
    status, output, _ = bounded_process.run(
        argv,
        cwd=ROOT,
        input_data=data,
        limits=bounded_process.Limits(timeout=60, stdout=8 * 1024**2, stderr=65536),
    )
    require(status == 0, "ruleset_command_failed")
    return output


def api(repository: str, path: str, body: JsonObject | None = None) -> JsonValue:
    """Call only the selected repository API; request bodies enter through stdin."""
    argv = [executable("gh"), "api", f"repos/{repository}/{path}".rstrip("/")]
    data = b""
    if body is not None:
        argv += ["--method", "PUT" if path.startswith("rulesets/") else "POST", "--input", "-"]
        data = json.dumps(body).encode()
    return decode_json(command(argv, data))


def load_policy() -> tuple[str, int, list[JsonObject]]:
    """Require direct-push history protection with no bypass or merge prerequisites."""
    policy = object_value(decode_json(bounded_file(POLICY, 65536)))
    require(set(policy) == {"repository", "repositoryId", "rulesets"}, "ruleset_policy_schema")
    repository = string_value(policy["repository"])
    identifier = policy["repositoryId"]
    require(
        re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository)
        and type(identifier) is int
        and identifier > 0,
        "ruleset_repository_identity",
    )
    entries = [object_value(item) for item in array_value(policy["rulesets"])]
    require(
        len(entries) == len(NAMES) and {string_value(item["name"]) for item in entries} == NAMES,
        "ruleset_names",
    )
    for entry in entries:
        require(set(entry) == FIELDS, "ruleset_fields")
        require(
            entry["target"] == "branch"
            and entry["enforcement"] == "active"
            and entry["conditions"]
            == {"ref_name": {"include": ["refs/heads/main"], "exclude": []}},
            "ruleset_target",
        )
        require(entry["bypass_actors"] == [], "history_bypass_forbidden")
        require(
            sorted(array_value(entry["rules"]), key=lambda item: json.dumps(item, sort_keys=True))
            == [{"type": "deletion"}, {"type": "non_fast_forward"}],
            "history_rules_differ",
        )
    return repository, int(str(identifier)), entries


def comparable(value: JsonObject) -> JsonObject:
    """Compare only API-controlled policy fields, ignoring response metadata and rule order."""
    result = {key: value[key] for key in FIELDS}
    result["rules"] = sorted(
        array_value(value["rules"]), key=lambda item: json.dumps(item, sort_keys=True)
    )
    return result


def inventory(repository: str) -> dict[str, JsonObject]:
    """Read full repository-owned records and reject duplicates or pagination truncation."""
    values = array_value(api(repository, "rulesets?includes_parents=false&per_page=100"))
    require(len(values) < PAGE_LIMIT, "ruleset_inventory_limit")
    result: dict[str, JsonObject] = {}
    for value in values:
        entry = object_value(value)
        name = string_value(entry["name"])
        if name not in NAMES | OBSOLETE_NAMES:
            continue
        identifier = entry["id"]
        require(
            type(identifier) is int and identifier > 0 and name not in result, "ruleset_ambiguous"
        )
        result[name] = object_value(api(repository, "rulesets/" + str(identifier)))
    return result


def healthy_checks(value: JsonObject, revision: str) -> None:
    """Reject similarly named statuses, wrong issuers and skipped checks."""
    checks = [object_value(item) for item in array_value(value["check_runs"])]
    require(value["total_count"] == len(checks), "ruleset_check_pagination")
    selected = [item for item in checks if item["name"] == GATE]
    require(len(selected) == 1, "ruleset_gate_missing_or_ambiguous")
    check = selected[0]
    require(
        check["head_sha"] == revision
        and check["status"] == "completed"
        and check["conclusion"] == "success"
        and object_value(check["app"])["id"] == GITHUB_ACTIONS,
        "ruleset_gate_not_successful",
    )


def healthy_analyses(value: JsonValue, revision: str) -> None:
    """Require the newest successful security analysis per language at the selected head."""
    entries = array_value(value)
    require(len(entries) <= PAGE_LIMIT, "ruleset_analysis_window")
    expected = {f"/language:{language}/security" for language in LANGUAGES}
    selected: dict[str, JsonObject] = {}
    for raw in entries:
        entry = object_value(raw)
        if object_value(entry["tool"]).get("name") != "CodeQL":
            continue
        category = string_value(entry["category"])
        if category in expected and category not in selected:
            selected[category] = entry
    require(set(selected) == expected, "ruleset_language_coverage")
    for entry in selected.values():
        require(
            entry.get("commit_sha") == revision and entry.get("ref") == "refs/heads/main",
            "ruleset_analysis_not_current",
        )
        require(
            entry.get("error") == "" and entry.get("warning") == "",
            "ruleset_analysis_failed",
        )
        count = entry.get("rules_count")
        require(type(count) is int and count > 0, "ruleset_analysis_empty")


def require_main_head(repository: str, revision: str) -> None:
    """Refuse a moved or differently typed main reference immediately before writes."""
    head = object_value(api(repository, "git/ref/heads/main"))
    target = object_value(head["object"])
    require(
        head.get("ref") == "refs/heads/main"
        and target.get("type") == "commit"
        and target.get("sha") == revision,
        "ruleset_remote_head_changed",
    )


def ready(repository: str, revision: str) -> None:
    """Prevent initial enforcement against an unverified or moving default branch."""
    require(re.fullmatch(r"[a-f0-9]{40}", revision), "ruleset_revision")
    require(
        command([executable("git"), "rev-parse", "HEAD"]).decode().strip() == revision,
        "ruleset_local_head",
    )
    require(
        not command([executable("git"), "status", "--porcelain=v1", "--untracked-files=all"]),
        "ruleset_dirty_checkout",
    )
    require_main_head(repository, revision)
    healthy_checks(
        object_value(api(repository, f"commits/{revision}/check-runs?per_page=100&filter=latest")),
        revision,
    )
    runs = object_value(
        api(repository, f"actions/workflows/ci.yml/runs?head_sha={revision}&event=push&per_page=10")
    )
    matching = [object_value(item) for item in array_value(runs["workflow_runs"])]
    require(bool(matching), "ruleset_trusted_ci_missing")
    latest = max(matching, key=lambda item: int(str(item["id"])))
    require(
        latest["head_sha"] == revision
        and latest["head_branch"] == "main"
        and latest["event"] == "push"
        and latest["status"] == "completed"
        and latest["conclusion"] == "success",
        "ruleset_trusted_ci_not_successful",
    )
    healthy_analyses(
        api(
            repository,
            "code-scanning/analyses?ref=refs%2Fheads%2Fmain&per_page=100"
            + "&tool_name=CodeQL&sort=created&direction=desc",
        ),
        revision,
    )
    for severity in ("critical", "high"):
        alerts = array_value(
            api(repository, f"code-scanning/alerts?state=open&severity={severity}&per_page=100")
        )
        require(not alerts, "ruleset_open_high_security_alerts_need_review")


@dataclass
class Options(argparse.Namespace):
    """Apply always names the already validated exact main commit explicitly."""

    mode: str = "plan"
    revision: str | None = None


def main(argv: Sequence[str] | None = None) -> int:
    """Print a bounded change summary; only apply can create or update the named policies."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("mode", choices=("plan", "check", "apply"))
    _ = parser.add_argument("--revision")
    args = parser.parse_args(argv, namespace=Options())
    try:
        repository, identifier, policies = load_policy()
        repo = object_value(api(repository, ""))
        require(
            repo["id"] == identifier and repo["default_branch"] == "main",
            "ruleset_repository_changed",
        )
        current = inventory(repository)
        obsolete = sorted(current.keys() & OBSOLETE_NAMES)
        changes = [
            item
            for item in policies
            if string_value(item["name"]) not in current
            or comparable(current[string_value(item["name"])]) != comparable(item)
        ]
        if args.mode == "apply":
            require(not obsolete, "ruleset_obsolete_policy_present")
            require(args.revision is not None, "ruleset_revision_required")
            ready(repository, args.revision or "")
            for policy in changes:
                require_main_head(repository, args.revision or "")
                existing = current.get(string_value(policy["name"]))
                endpoint = "rulesets" if existing is None else "rulesets/" + str(existing["id"])
                _ = api(repository, endpoint, policy)
            require_main_head(repository, args.revision or "")
            current = inventory(repository)
            require_main_head(repository, args.revision or "")
            obsolete = sorted(current.keys() & OBSOLETE_NAMES)
            require(not obsolete, "ruleset_obsolete_policy_present")
            require(
                all(
                    comparable(current[string_value(item["name"])]) == comparable(item)
                    for item in policies
                ),
                "ruleset_readback_differs",
            )
        _ = sys.stdout.write(
            json.dumps(
                {
                    "mode": args.mode,
                    "repository": repository,
                    "changes": [item["name"] for item in changes],
                    "obsoleteRulesets": obsolete,
                    "matches": not obsolete and (not changes or args.mode == "apply"),
                }
            )
            + "\n"
        )
    except (
        ToolError,
        ValueError,
        OSError,
        KeyError,
        RuntimeError,
        bounded_process.ProcessError,
    ) as error:
        code = str(error) if isinstance(error, ToolError) else type(error).__name__
        _ = sys.stderr.write(f"Ruleset reconciliation failed: {code}\n")
        return 1
    return 1 if args.mode == "check" and (changes or obsolete) else 0


if __name__ == "__main__":
    raise SystemExit(main())
