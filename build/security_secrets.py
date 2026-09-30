"""Scan all selected source bytes without inherited filename exclusions.

Gitleaks' default allowlists omit lockfiles, binary extensions and dependency
directories. A neutral-name copy removes that blind spot while an authenticated
map retains the original source identity. Reviewed fixture exceptions are mapped
to their exact neutral filename; no value receives a repository-wide exemption.
"""

from __future__ import annotations

import hashlib
import json
import re
import tomllib
from typing import TYPE_CHECKING, cast

if TYPE_CHECKING:
    from pathlib import Path

from security_context import MAX_SOURCE, Context
from security_findings import list_value, object_value
from security_tools import bounded_file, require, string, write_private


def neutral_snapshot(context: Context, source: Path, config: Path) -> tuple[Path, Path]:
    """Preserve every selected byte and translate only the reviewed fixture path constraints."""
    manifest = object_value(
        cast(
            "object", json.loads(bounded_file(context.output / "source-manifest.json", 4 * 1024**2))
        )
    )
    destination = context.output / "secret-inputs"
    destination.mkdir(mode=0o700)
    mapped: dict[str, str] = {}
    for index, (name, expected) in enumerate(sorted(manifest.items())):
        path = source / name
        content = bounded_file(path, MAX_SOURCE) if path.stat().st_size else b""
        require(hashlib.sha256(content).hexdigest() == expected, "secret_snapshot_changed")
        neutral = f"source{index:06d}"
        write_private(destination / neutral, content, 0o600)
        mapped[name] = neutral
    write_private(
        context.output / "secret-paths.json",
        (json.dumps(mapped, sort_keys=True, indent=2) + "\n").encode(),
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
