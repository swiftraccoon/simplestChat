"""Prune obsolete allowlisted GitHub caches after successful main CI; default to dry-run."""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from urllib.parse import urlencode

from security_context import executable
from security_tools import ToolError, require

# isort: split
import bounded_process
from release_json import (
    JsonObject,
    JsonValue,
    array_value,
    decode_json,
    integer_value,
    object_value,
    string_value,
)

REPOSITORY = "swiftraccoon/simplestChat"
REFERENCE = "refs/heads/main"
MAX_CACHES = 1000
MAX_DELETIONS = 100
MAX_TEXT = 512
MAX_TIMESTAMP = 40
PAGE_SIZE = 100
SOFT_BYTES = 8 * 1024**3
RECENT = timedelta(hours=2)
DIGEST = r"[a-f0-9]{64}"
NAMESPACE = r"(?:main|untrusted)-[a-zA-Z0-9_]+-[a-zA-Z0-9_]+"


def family(key: str) -> tuple[str, int] | None:
    """Keep unknown caches untouched and retain separate explicit trust/OS/ISA namespaces."""
    match = re.fullmatch(r"buildx-v[23]-(" + NAMESPACE + ")-" + DIGEST, key)
    if match:
        return "buildx-" + match[1], 1
    match = re.fullmatch(r"codeql-analyzed-v2-(rust|c-cpp)-(" + NAMESPACE + ")-" + DIGEST, key)
    if match:
        return "codeql-" + match[1] + "-" + match[2], 2
    match = re.fullmatch(r"codeql-analyzed-v1-(rust|c-cpp)-" + DIGEST, key)
    if match:
        # Migration only: every hosted v1 database here was produced on Linux
        # AMD64 main. v2 exposes this namespace so future ISAs remain separate.
        return "codeql-" + match[1] + "-main-linux-x86_64", 2
    match = re.fullmatch(r"grype-db-v1-(main|untrusted)-" + DIGEST + r"-\d{4}-\d{2}-\d{2}", key)
    if match:
        # Grype's database is architecture-independent, including across CLI pins.
        return "grype-" + match[1], 2
    patterns = (
        r"(codeql-rust-cargo-v1-" + NAMESPACE + ")-" + DIGEST,
        r"((?:codeql-cli-v1|codeql-openssl-v1|openssl-v3|native-dtls-v2)-"
        + NAMESPACE
        + ")-"
        + DIGEST,
        r"(vendored-v2-" + NAMESPACE + r"-rust-(?:check|build|test))-" + DIGEST,
        r"(native-prepared-v1-(?:main|untrusted)-(?:amd64|arm64))-" + DIGEST,
        r"(native-compiled-v1-(?:asan|ubsan|replay)-(?:main|untrusted)-(?:amd64|arm64))-" + DIGEST,
        r"(v3-rust-(?:main|untrusted)-(?:X64|ARM64)-[a-z-]{1,40}-(?:Linux|macOS|Windows)-(?:x64|arm64))-[a-f0-9]{8}-[a-f0-9]{8}",
    )
    for pattern in patterns:
        match = re.fullmatch(pattern, key)
        if match:
            return match[1], 2
    return None


def timestamp(value: JsonValue) -> datetime:
    """Require an explicit UTC cache timestamp."""
    text = string_value(value)
    require(len(text) <= MAX_TIMESTAMP and text.endswith("Z"), "cache_retention_timestamp")
    return datetime.fromisoformat(text.removesuffix("Z") + "+00:00")


@dataclass(frozen=True)
class Entry:
    """Represent only the immutable identity and freshness needed for a cache deletion."""

    identifier: int
    key: str
    reference: str
    version: str
    size: int
    created: datetime
    accessed: datetime

    @classmethod
    def parse(cls, value: JsonValue) -> Entry:
        """Reject malformed metadata before planning any mutation."""
        raw = object_value(value)
        result = cls(
            integer_value(raw["id"]),
            string_value(raw["key"]),
            string_value(raw["ref"]),
            string_value(raw["version"]),
            integer_value(raw["size_in_bytes"]),
            timestamp(raw["created_at"]),
            timestamp(raw["last_accessed_at"]),
        )
        require(
            result.identifier > 0
            and 0 <= result.size <= 100 * 1024**3
            and 0 < len(result.key) <= MAX_TEXT
            and len(result.reference) <= MAX_TEXT
            and re.fullmatch(DIGEST, result.version),
            "cache_retention_metadata",
        )
        return result

    def recent(self, now: datetime) -> bool:
        """Protect caches newly created or used, including future timestamps from clock skew."""
        return max(self.created, self.accessed) >= now - RECENT

    def summary(self) -> JsonObject:
        """Describe concrete disposable cache entries without printing credentials."""
        return {"id": self.identifier, "key": self.key, "bytes": self.size}


def plan(entries: list[Entry], now: datetime) -> list[Entry]:
    """Keep current namespaces and recent entries before applying the soft storage target."""
    groups: dict[str, list[Entry]] = {}
    limits: dict[str, int] = {}
    for entry in entries:
        selected = family(entry.key) if entry.reference == REFERENCE else None
        if selected is not None:
            name, keep = selected
            groups.setdefault(name, []).append(entry)
            limits[name] = keep
    removals: list[Entry] = []
    optional: list[Entry] = []
    for name, group in groups.items():
        ordered = sorted(group, key=lambda item: (item.created, item.identifier), reverse=True)
        for index, entry in enumerate(ordered):
            if index == 0 or entry.recent(now):
                continue
            if index >= limits[name]:
                removals.append(entry)
            else:
                optional.append(entry)
    retained_bytes = sum(entry.size for entry in entries) - sum(entry.size for entry in removals)
    for entry in sorted(optional, key=lambda item: (item.accessed, item.created, item.identifier)):
        if retained_bytes <= SOFT_BYTES:
            break
        removals.append(entry)
        retained_bytes -= entry.size
    return sorted(removals, key=lambda item: (item.accessed, item.identifier))[:MAX_DELETIONS]


class Github:
    """Limit API reads and deletions to this repository's cache IDs and trusted CI metadata."""

    def __init__(self, *, apply: bool = False) -> None:
        """Use a bounded GitHub CLI session with no alternate host or repository selection."""
        self.apply: bool = apply
        self.deadline: float = time.monotonic() + 240
        self.deleted: list[int] = []

    def request(self, suffix: str, *, delete: bool = False) -> JsonValue:
        """Never issue a source, artifact, workflow-run or repository deletion."""
        require(
            (
                not delete
                and (
                    suffix == "git/ref/heads/main"
                    or re.fullmatch(r"actions/runs/[0-9]+", suffix)
                    or suffix.startswith("actions/caches?")
                )
            )
            or (
                delete
                and self.apply
                and len(self.deleted) < MAX_DELETIONS
                and re.fullmatch(r"actions/caches/[0-9]+", suffix)
            ),
            "cache_retention_endpoint",
        )
        remaining = self.deadline - time.monotonic()
        require(remaining > 0, "cache_retention_deadline")
        env = {name: value for name, value in os.environ.items() if name != "GH_DEBUG"}
        env.update({"GH_PROMPT_DISABLED": "1", "GH_HOST": "github.com", "GH_PAGER": "cat"})
        status, output, _ = bounded_process.run(
            [
                executable("gh"),
                "api",
                "--hostname",
                "github.com",
                "--method",
                "DELETE" if delete else "GET",
                "-H",
                "Accept: application/vnd.github+json",
                "-H",
                "X-GitHub-Api-Version: 2022-11-28",
                f"repos/{REPOSITORY}/{suffix}",
            ],
            env=env,
            limits=bounded_process.Limits(
                timeout=min(20, remaining), stdout=4 * 1024**2, stderr=65536
            ),
        )
        require(status == 0, "cache_retention_api")
        return None if delete else decode_json(output)

    def inventory(self, key: str | None = None) -> list[Entry]:
        """Read a bounded complete inventory; reject partial pages or a shifted list."""
        entries: list[Entry] = []
        total: int | None = None
        for page in range(1, MAX_CACHES // PAGE_SIZE + 1):
            query: dict[str, str | int] = {
                "ref": REFERENCE,
                "per_page": PAGE_SIZE,
                "page": page,
                "sort": "created_at",
                "direction": "desc",
            }
            if key is not None:
                query["key"] = key
            raw = object_value(self.request("actions/caches?" + urlencode(query)))
            current_total = integer_value(raw["total_count"])
            require(
                0 <= current_total <= MAX_CACHES and total in {None, current_total},
                "cache_retention_count",
            )
            total = current_total
            batch = array_value(raw["actions_caches"])
            require(len(batch) <= PAGE_SIZE, "cache_retention_page")
            entries.extend(Entry.parse(value) for value in batch)
            require(
                len({entry.identifier for entry in entries}) == len(entries),
                "cache_retention_duplicate",
            )
            if len(entries) == total:
                return entries
            require(batch and len(entries) < total, "cache_retention_incomplete")
        raise ToolError("cache_retention_pages")  # noqa: EM101 -- Fixed safe code.

    def trusted_run(self, run_id: int, revision: str) -> None:
        """Require completed successful push CI from this repository's exact main revision."""
        require(run_id > 0 and re.fullmatch(r"[a-f0-9]{40}", revision), "cache_retention_run")
        run = object_value(self.request("actions/runs/" + str(run_id)))
        require(
            all(
                run.get(key) == value
                for key, value in {
                    "id": run_id,
                    "name": "CI",
                    "event": "push",
                    "status": "completed",
                    "conclusion": "success",
                    "head_branch": "main",
                    "head_sha": revision,
                    "path": ".github/workflows/ci.yml",
                }.items()
            )
            and object_value(run["repository"]).get("full_name") == REPOSITORY
            and object_value(run["head_repository"]).get("full_name") == REPOSITORY,
            "cache_retention_untrusted_run",
        )
        self.current_head(revision)

    def current_head(self, revision: str) -> None:
        """Prevent an old completion from cleaning caches while a newer main revision runs."""
        reference = object_value(self.request("git/ref/heads/main"))
        require(
            object_value(reference["object"]).get("sha") == revision, "cache_retention_stale_run"
        )

    def remove(self, entry: Entry, revision: str) -> bool:
        """Refresh exact identity and recent use immediately before deleting one cache ID."""
        require(
            self.apply and entry.reference == REFERENCE and family(entry.key),
            "cache_retention_apply",
        )
        self.current_head(revision)
        refreshed = self.inventory()
        # Another cache eviction can remove the entries we planned to keep.
        # Re-plan the complete inventory before deleting any remaining copy.
        if entry.identifier not in {item.identifier for item in plan(refreshed, datetime.now(UTC))}:
            return False
        matches = [item for item in refreshed if item.identifier == entry.identifier]
        if not matches:
            return False
        current = matches[0]
        require(
            (current.key, current.reference, current.version, current.size, current.created)
            == (entry.key, entry.reference, entry.version, entry.size, entry.created),
            "cache_retention_changed",
        )
        if current.recent(datetime.now(UTC)):
            return False
        _ = self.request("actions/caches/" + str(current.identifier), delete=True)
        self.deleted.append(current.identifier)
        return True


class Options(argparse.Namespace):
    """Keep dry-run the default and require a concrete successful run before apply."""

    apply: bool = False
    run_id: int = 0
    revision: str = ""


def main() -> int:
    """Print a reviewable retention plan and optionally apply bounded exact-ID cache deletions."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--apply", action="store_true")
    _ = parser.add_argument("--run-id", type=int, default=0)
    _ = parser.add_argument("--revision", default="")
    args = parser.parse_args(namespace=Options())
    client = Github(apply=args.apply)
    try:
        if args.apply:
            client.trusted_run(args.run_id, args.revision)
        entries = client.inventory()
        selected = plan(entries, datetime.now(UTC))
        before = sum(entry.size for entry in entries)
        proposed = before - sum(entry.size for entry in selected)
        result: JsonObject = {
            "repository": REPOSITORY,
            "ref": REFERENCE,
            "dryRun": not args.apply,
            "bytesBefore": before,
            "bytesAfterPlan": proposed,
            "softLimitBytes": SOFT_BYTES,
            "protectedBytesExceedLimit": proposed > SOFT_BYTES,
            "delete": [entry.summary() for entry in selected],
        }
        _ = sys.stdout.write(json.dumps(result, sort_keys=True) + "\n")
        if args.apply:
            for entry in selected:
                _ = client.remove(entry, args.revision)
            _ = sys.stdout.write(json.dumps({"deletedIds": client.deleted}) + "\n")
    except (ToolError, OSError, ValueError, KeyError):
        _ = sys.stderr.write(
            "Cache retention stopped; no source, runs or artifacts were targeted.\n"
        )
        _ = sys.stdout.write(json.dumps({"deletedIds": client.deleted}) + "\n")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
