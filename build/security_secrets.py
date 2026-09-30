"""Project selected source bytes without filename or binary-content exclusions.

Gitleaks' default allowlists omit lockfiles, binary extensions and dependency
directories. A neutral-name copy removes that blind spot while an authenticated
map retains the original source identity. A bounded ASCII projection prevents
content-based binary exclusions. Reviewed fixture exceptions are mapped to their
exact neutral filename; no value receives a repository-wide exemption.
"""

from __future__ import annotations

import hashlib
import json
import re
import shutil
import stat
import tomllib
from typing import TYPE_CHECKING, cast

if TYPE_CHECKING:
    from pathlib import Path

from security_context import MAX_FILES, MAX_SOURCE, MAX_TREE, Context
from security_findings import list_value, object_value
from security_secret_projection import LIMITATIONS, PREFIX, project
from security_tools import bounded_file, require, string, write_private

DISK_RESERVE = 64 * 1024**2


def projection_budget(context: Context, source: Path, manifest: dict[str, object]) -> int:
    """Require capacity for every projection before creating any projected file."""
    require(0 < len(manifest) <= MAX_FILES, "secret_source_count")
    total = 0
    for name in manifest:
        metadata = (source / name).lstat()
        require(stat.S_ISREG(metadata.st_mode), "secret_source_kind")
        require(0 <= metadata.st_size <= MAX_SOURCE, "secret_source_size")
        total += metadata.st_size
        require(total <= MAX_TREE, "secret_tree_size")
    projected = total + len(manifest) * len(PREFIX)
    require(
        shutil.disk_usage(context.output).free >= projected + DISK_RESERVE,
        "secret_projection_disk_space",
    )
    return projected


def neutral_snapshot(context: Context, source: Path, config: Path) -> tuple[Path, Path]:
    """Bind every projection to original bytes and preserve reviewed fixture path constraints."""
    manifest_bytes = bounded_file(context.output / "source-manifest.json", 4 * 1024**2)
    manifest = object_value(cast("object", json.loads(manifest_bytes)))
    expected_bytes = projection_budget(context, source, manifest)
    destination = context.output / "secret-inputs"
    destination.mkdir(mode=0o700)
    mapped: dict[str, str] = {}
    coverage: dict[str, object] = {}
    total = 0
    projected_total = 0
    for index, (name, expected) in enumerate(sorted(manifest.items())):
        path = source / name
        neutral = f"source{index:06d}"
        projection = project(path, destination / neutral, max_bytes=MAX_SOURCE)
        require(projection.source_sha256 == expected, "secret_snapshot_changed")
        total += projection.source_bytes
        projected_total += projection.projection_bytes
        mapped[name] = neutral
        coverage[neutral] = {
            "path": name,
            "sha256": projection.source_sha256,
            "sourceBytes": projection.source_bytes,
            "projectionSha256": projection.projection_sha256,
            "projectionBytes": projection.projection_bytes,
            "projectionFormat": projection.format,
        }
    require(projected_total == expected_bytes, "secret_projection_coverage_changed")
    write_private(
        context.output / "secret-paths.json",
        (json.dumps(mapped, sort_keys=True, indent=2) + "\n").encode(),
        0o600,
    )
    write_private(
        context.output / "secret-coverage.json",
        (
            json.dumps(
                {
                    "schemaVersion": 1,
                    "sourceManifestSha256": hashlib.sha256(manifest_bytes).hexdigest(),
                    "files": len(coverage),
                    "sourceBytes": total,
                    "projectionBytes": projected_total,
                    "entries": coverage,
                    "limitations": [
                        *LIMITATIONS,
                        (
                            "Git history checks cover textual diffs; "
                            "projection covers the selected current tree"
                        ),
                    ],
                },
                sort_keys=True,
                indent=2,
            )
            + "\n"
        ).encode(),
        0o600,
    )
    raw = object_value(cast("object", tomllib.loads(bounded_file(config, 1024 * 1024).decode())))
    lines = [
        "title = 'Reviewed source fixtures mapped to neutral paths'",
        "[extend]",
        "useDefault = true",
        "",
    ]
    for value in list_value(raw["allowlists"]):
        entry = object_value(value)
        patterns = [string(item) for item in list_value(entry["paths"])]
        matches = [
            neutral
            for name, neutral in mapped.items()
            if any(re.search(pattern, name) for pattern in patterns)
        ]
        if not matches:
            continue  # A deleted fixture has no current content to exempt.
        require(len(matches) == 1, "ambiguous_fixture_source")
        derived = {**entry, "paths": ["(^|/)" + matches[0] + "$"]}
        lines.extend(
            [
                "[[allowlists]]",
                *(key + " = " + json.dumps(item) for key, item in derived.items()),
                "",
            ]
        )
    translated = context.output / "secret-configuration.toml"
    write_private(translated, ("\n".join(lines) + "\n").encode(), 0o600)
    return destination, translated
