"""Bound scanner processes and construct a source-only, private scan snapshot.

Reports can contain source excerpts and are kept local under a private directory.
Only fixed failure codes and compact check summaries should reach CI artifacts.
The shared operations process boundary enforces limits while reading each stream
and terminates the owned process group on timeout, overflow or cancellation.
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, cast

from security_tools import bounded_file, require, safe_name, write_private

if TYPE_CHECKING:
    from collections.abc import Mapping, Sequence

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ops/ansible/files"))
import bounded_process

ROOT = Path(__file__).resolve().parents[1]
MAX_SOURCE = 16 * 1024**2
MAX_TREE = 128 * 1024**2
MAX_FILES = 20000
MAX_REPORT = 16 * 1024**2


def json_object(data: bytes) -> dict[str, object]:
    """Decode a bounded command result while rejecting a different root type."""
    raw = cast("object", json.loads(data))
    require(isinstance(raw, dict), "scanner_report_object")
    return cast("dict[str, object]", raw)


def executable(name: str) -> str:
    """Resolve a prerequisite once; scanner binaries use security_tools instead."""
    path = shutil.which(name)
    require(path is not None, "missing_security_prerequisite_" + name)
    return cast("str", path)


def environment(output: Path) -> dict[str, str]:
    """Do not forward CI credentials, Python hooks or scanner configuration overrides."""
    home = output / "home"
    home.mkdir(mode=0o700)
    result = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": str(home),
        "LANG": "C.UTF-8" if sys.platform == "linux" else "en_US.UTF-8",
        "LC_ALL": "C.UTF-8" if sys.platform == "linux" else "en_US.UTF-8",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_CONFIG_SYSTEM": os.devnull,
        "GIT_TERMINAL_PROMPT": "0",
        "PIP_CONFIG_FILE": os.devnull,
        "PIP_DISABLE_PIP_VERSION_CHECK": "1",
        "PYTHONNOUSERSITE": "1",
        "PYTHONDONTWRITEBYTECODE": "1",
        "CARGO_HOME": os.environ.get("CARGO_HOME", str(Path.home() / ".cargo")),
        "RUSTUP_HOME": os.environ.get("RUSTUP_HOME", str(Path.home() / ".rustup")),
        "SEMGREP_SEND_METRICS": "off",
        "NO_COLOR": "1",
    }
    for name in ("SSL_CERT_FILE", "SSL_CERT_DIR", "TMPDIR"):
        if name in os.environ:
            result[name] = os.environ[name]
    return result


@dataclass
class Context:
    """One invocation owns its bounded command evidence and immutable source copy."""

    root: Path
    output: Path
    env: dict[str, str] = field(default_factory=dict)
    sequence: int = 0
    checks: list[dict[str, object]] = field(default_factory=list)

    def __post_init__(self) -> None:
        """Initialize the credential-free child environment once."""
        self.env = environment(self.output)

    def run(  # noqa: PLR0913 -- Explicit independent process limits and report disposition.
        self,
        name: str,
        argv: Sequence[str],
        *,
        cwd: Path | None = None,
        timeout: int = 180,
        accepted: tuple[int, ...] = (0,),
        input_data: bytes = b"",
        env_updates: Mapping[str, str] | None = None,
    ) -> tuple[int, bytes]:
        """Retain separate bounded streams; never echo report contents or raw errors."""
        require(safe_name(name) and "/" not in name, "invalid_scanner_check_name")
        self.sequence += 1
        prefix = self.output / f"{self.sequence:02d}-{name}"
        started = time.monotonic()
        with (
            prefix.with_suffix(".stdout").open("xb") as output,
            prefix.with_suffix(".stderr").open("xb") as error,
        ):
            status, _, _ = bounded_process.run(
                argv,
                cwd=cwd or self.root,
                env={**self.env, **(env_updates or {})},
                limits=bounded_process.Limits(
                    timeout=timeout, stdout=MAX_REPORT, stderr=MAX_REPORT
                ),
                input_data=input_data,
                output=output,
                error=error,
            )
        self.checks.append(
            {
                "name": name,
                "exitStatus": status,
                "elapsedSeconds": round(time.monotonic() - started, 3),
            }
        )
        require(status in accepted, "scanner_command_failed_" + name)
        path = prefix.with_suffix(".stdout")
        return status, bounded_file(path, MAX_REPORT) if path.stat().st_size else b""

    def snapshot(self, label: str = "source") -> Path:
        """Copy tracked and nonignored files, rejecting links and bounding total bytes."""
        require(
            safe_name(label) and "/" not in label and "." not in label,
            "invalid_source_snapshot_label",
        )
        _, listing = self.run(
            label + "-inventory",
            [executable("git"), "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
        )
        names = sorted(set(listing.decode("utf-8").rstrip("\0").split("\0")))
        require(0 < len(names) <= MAX_FILES, "security_source_count")
        destination = self.output / label
        destination.mkdir(mode=0o700)
        total = 0
        identities: dict[str, str] = {}
        for name in names:
            require(safe_name(name), "security_source_path")
            source = self.root / name
            if not source.exists() and not source.is_symlink():
                continue  # A tracked deletion has no current bytes to scan.
            require(
                all(
                    not parent.is_symlink()
                    for parent in source.parents
                    if parent != self.root and self.root in parent.parents
                ),
                "security_source_parent_link",
            )
            size = source.lstat().st_size
            if size == 0:
                content = b""
                require(source.is_file() and not source.is_symlink(), "security_source_type")
            else:
                content = bounded_file(source, MAX_SOURCE)
            total += len(content)
            require(total <= MAX_TREE, "security_source_size")
            target = destination / name
            target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            write_private(target, content, 0o600)
            identities[name] = hashlib.sha256(content).hexdigest()
        write_private(
            self.output / (label + "-manifest.json"),
            (json.dumps(identities, sort_keys=True, indent=2) + "\n").encode(),
            0o600,
        )
        return destination
