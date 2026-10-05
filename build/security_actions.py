"""Review changed action pins against public advisory and exact-ref license metadata."""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, cast
from urllib.parse import urlencode

import yaml
from security_context import ROOT, Context, executable
from security_dependency_licenses import allowed_licenses
from security_image_policy import LicenseExpression
from security_source_scope import require_local_vendor, selected
from security_tools import ToolError, bounded_file, require, write_private

# isort: split
import bounded_process
from release_json import JsonObject, JsonValue, array_value, decode_json, object_value, string_value

if TYPE_CHECKING:
    from collections.abc import Sequence

MAX_BYTES = 4 * 1024**2
MAX_NODES = 20000
MAX_WORKFLOWS = 500
PAGE_SIZE = 100
MAX_VERSIONS = 16
MAX_CHANGED = 64
PIN = re.compile(r"([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)(/[A-Za-z0-9_./-]+)?@([a-f0-9]{40})")
VERSION = re.compile(r"v?\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?(?:\+[A-Za-z0-9.-]+)?")


def command(argv: Sequence[str]) -> bytes:
    """Bound public metadata reads without reflecting command errors or credentials."""
    status, output, _ = bounded_process.run(
        argv,
        cwd=ROOT,
        limits=bounded_process.Limits(timeout=45, stdout=MAX_BYTES, stderr=65536),
    )
    require(status == 0, "actions_metadata_command_failed")
    return output


def api(path: str, parameters: dict[str, str]) -> JsonValue:
    """Read fixed-origin public GitHub metadata without credentials or redirects."""
    require(path.startswith("/") and "?" not in path, "actions_api_path")
    url = "https://api.github.com" + path + "?" + urlencode(parameters)
    output = command(
        [
            executable("curl"),
            "--disable",
            "--fail",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--connect-timeout",
            "10",
            "--max-time",
            "30",
            "--max-filesize",
            str(MAX_BYTES),
            "--header",
            "Accept: application/vnd.github+json",
            "--header",
            "X-GitHub-Api-Version: 2022-11-28",
            "--header",
            "User-Agent: simplestChat-local-ci",
            url,
        ]
    )
    return decode_json(output)


def pins(data: bytes) -> set[str]:
    """Collect every literal uses value, including composite and reusable actions."""
    require(len(data) <= MAX_BYTES, "actions_workflow_size")
    pending: list[object] = [yaml.safe_load(data)]
    seen: set[int] = set()
    result: set[str] = set()
    while pending:
        value = pending.pop()
        identity = id(value)
        if not isinstance(value, (dict, list)) or identity in seen:
            continue
        seen.add(identity)
        require(len(seen) <= MAX_NODES, "actions_workflow_complexity")
        if isinstance(value, list):
            pending.extend(cast("list[object]", value))
            continue
        for key, item in cast("dict[object, object]", value).items():
            if key != "uses":
                pending.append(item)
                continue
            require(isinstance(item, str), "actions_use_not_literal")
            literal = cast("str", item)
            if literal.startswith("./"):
                continue
            require(PIN.fullmatch(literal) is not None, "actions_unpinned_or_unsupported_use")
            result.add(literal)
    return result


def inventory(snapshot: Path | None, base: str, *, include_vendor: bool = False) -> set[str]:
    """Compare maintained workflow/action YAML, not generated or ignored files."""
    if snapshot is None:
        names = command([executable("git"), "ls-tree", "-rz", "--name-only", base])
        candidates = names.decode().split("\0")
    else:
        candidates = [
            str(path.relative_to(snapshot)) for path in snapshot.rglob("*") if path.is_file()
        ]
    files = {
        name
        for name in candidates
        if selected(name, include_vendor=include_vendor)
        and (
            (name.startswith(".github/") and name.endswith((".yml", ".yaml")))
            or Path(name).name in {"action.yml", "action.yaml"}
        )
    }
    require(len(files) <= MAX_WORKFLOWS, "actions_workflow_count")
    result: set[str] = set()
    for name in sorted(files):
        if snapshot is None:
            data = command([executable("git"), "show", base + ":" + name])
        else:
            path = snapshot / name
            require(
                path.resolve().is_relative_to(snapshot.resolve()), "actions_source_outside_snapshot"
            )
            data = bounded_file(path, MAX_BYTES)
        result.update(pins(data))
    return result


def advisories(package: str, version: str | None = None) -> list[JsonObject]:
    """Check all nonwithdrawn reviewed advisories and malware, at every severity."""
    selected: list[JsonObject] = []
    for kind in ("reviewed", "malware"):
        data = array_value(
            api(
                "/advisories",
                {
                    "ecosystem": "actions",
                    "affects": package + ("@" + version if version else ""),
                    "type": kind,
                    "is_withdrawn": "false",
                    "per_page": str(PAGE_SIZE),
                },
            )
        )
        require(len(data) < PAGE_SIZE, "actions_advisory_page_incomplete")
        for raw in data:
            item = object_value(raw)
            require(isinstance(item.get("ghsa_id"), str), "actions_advisory_schema")
            require(item.get("withdrawn_at") is None, "actions_withdrawn_filter_failed")
            selected.append(item)
    return selected


def release_versions(repository: str, revision: str) -> list[str]:
    """Bind advisory versions to real release tags at the exact selected action commit."""
    output = command(
        [executable("git"), "ls-remote", "--tags", "https://github.com/" + repository + ".git"]
    )
    tags: dict[str, str] = {}
    peeled: dict[str, str] = {}
    for line in output.decode().splitlines():
        commit, name = line.split("\t", 1)
        require(re.fullmatch(r"[a-f0-9]{40}", commit), "actions_tag_identity")
        require(name.startswith("refs/tags/"), "actions_tag_ref")
        tag = name.removeprefix("refs/tags/")
        if tag.endswith("^{}"):
            peeled[tag.removesuffix("^{}")] = commit
        else:
            tags[tag] = commit
    tags.update(peeled)
    versions = sorted(
        {
            tag.removeprefix("v")
            for tag, commit in tags.items()
            if commit == revision and VERSION.fullmatch(tag)
        }
    )
    require(0 < len(versions) <= MAX_VERSIONS, "actions_advisory_version_unresolved")
    return versions


def review(pin: str, allowed: set[str]) -> JsonObject:
    """Require an allowed exact-ref license and no current finding for the source pin."""
    match = PIN.fullmatch(pin)
    require(match is not None, "actions_pin_identity")
    if match is None:
        code = "actions_pin_identity"
        raise ToolError(code)
    repository, suffix, revision = match.groups()
    repository = repository.lower()
    metadata = object_value(api("/repos/" + repository + "/license", {"ref": revision}))
    license_id = string_value(object_value(metadata["license"])["spdx_id"])
    require(LicenseExpression(license_id, allowed).allowed_expression(), "actions_license_blocked")
    versions: list[str] = []
    packages = sorted({repository, repository + (suffix or "")})
    for package in packages:
        if advisories(package):
            versions = versions or release_versions(repository, revision)
            for version in versions:
                require(not advisories(package, version), "actions_advisory_blocked")
    return {
        "pin": pin,
        "license": license_id,
        "versions": list[JsonValue](versions),
        "passed": True,
    }


def check(
    context: Context, snapshot: Path, base: str | None, *, include_vendor: bool = False
) -> None:
    """Run this review through the same bounded process/evidence path as other scanners."""
    _ = context.run(
        "actions-dependency-review",
        [
            sys.executable,
            str(ROOT / "build/security_actions.py"),
            "--snapshot",
            str(snapshot),
            "--base",
            base or "HEAD",
            "--output",
            str(context.output / "actions-dependencies.json"),
            *(["--include-vendor"] if include_vendor else []),
        ],
        timeout=600,
    )


@dataclass
class Options(argparse.Namespace):
    """Require one bounded snapshot and explicit comparison base."""

    snapshot: Path = Path()
    base: str = ""
    output: Path = Path()
    include_vendor: bool = False


def main() -> int:
    """Review only newly introduced or changed immutable action pins."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--snapshot", required=True, type=Path)
    _ = parser.add_argument("--base", required=True)
    _ = parser.add_argument("--output", required=True, type=Path)
    _ = parser.add_argument("--include-vendor", action="store_true")
    args = parser.parse_args(namespace=Options())
    try:
        require_local_vendor(include_vendor=args.include_vendor)
        require(
            args.base == "HEAD" or re.fullmatch(r"[a-f0-9]{40}", args.base), "actions_invalid_base"
        )
        _ = command([executable("git"), "merge-base", "--is-ancestor", args.base, "HEAD"])
        added = sorted(
            inventory(args.snapshot, args.base, include_vendor=args.include_vendor)
            - inventory(None, args.base, include_vendor=args.include_vendor)
        )
        require(len(added) <= MAX_CHANGED, "actions_changed_pin_limit")
        allowed = allowed_licenses(args.snapshot)
        records: list[JsonValue] = [review(pin, allowed) for pin in added]
        write_private(
            args.output, (json.dumps({"passed": True, "pins": records}) + "\n").encode(), 0o600
        )
    except (
        ToolError,
        OSError,
        ValueError,
        KeyError,
        yaml.YAMLError,
        bounded_process.ProcessError,
    ) as error:
        code = str(error) if isinstance(error, ToolError) else type(error).__name__
        _ = sys.stderr.write(f"Action dependency review failed: {code}\n")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
