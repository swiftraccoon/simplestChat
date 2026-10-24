"""Fixed guest-only bridge to the authenticated installed receiver.

The product client command is unchanged. This source is staged only in the
marked disposable guest, and never accepts a command, root, tool or URL selector.
It leaves both protocol frames on stdin for the original native receiver.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import stat
import sys
from pathlib import Path
from typing import NoReturn, cast

ROOT = Path("/root/simplestchat-vm-fixture")
CONFIG = ROOT / "receive-route.json"
MARKER = Path("/etc/simplestchat-vm-fixture")
BINARY = Path("/usr/local/libexec/simplestchat-public/simplestchat-ops")
MAX_BINARY = 256 * 1024**2
EXECUTABLE_MODE = 0o755


def require(condition: object) -> None:
    """Never print an external value or a private protocol frame."""
    if not condition:
        message = "receive_vm_route_unproven"
        raise ValueError(message)


def stamp(metadata: os.stat_result) -> tuple[int, ...]:
    """Ignore read-only atime while retaining identity, ownership and mutation."""
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_mode,
        metadata.st_uid,
        metadata.st_gid,
        metadata.st_nlink,
        metadata.st_size,
        metadata.st_mtime_ns,
        metadata.st_ctime_ns,
    )


def read_original(path: Path, maximum: int, mode: int) -> bytes:
    """Read the same nofollow regular inode and close it even on refusal."""
    descriptor = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as source:
        original = os.fstat(source.fileno())
        require(
            stat.S_ISREG(original.st_mode)
            and original.st_uid == 0
            and stat.S_IMODE(original.st_mode) == mode
            and 0 < original.st_size <= maximum
            and stamp(path.lstat()) == stamp(original)
        )
        data = source.read(maximum + 1)
        require(
            len(data) == original.st_size
            and stamp(os.fstat(source.fileno())) == stamp(original)
            and stamp(path.lstat()) == stamp(original)
        )
        return data


def config_value(data: bytes) -> dict[str, object]:
    """Accept only the fixed controller-installed route pin."""

    def pairs(items: list[tuple[str, object]]) -> dict[str, object]:
        value: dict[str, object] = {}
        for key, item in items:
            require(key not in value)
            value[key] = item
        return value

    raw = cast("object", json.loads(data, object_pairs_hook=pairs))
    require(isinstance(raw, dict))
    value = cast("dict[str, object]", raw)
    require(set(value) == {"schemaVersion", "runId", "binarySha256", "binaryBytes"})
    require(
        type(value["schemaVersion"]) is int
        and value["schemaVersion"] == 1
        and isinstance(value["runId"], str)
        and re.fullmatch("[a-f0-9]{32}", value["runId"])
        and isinstance(value["binarySha256"], str)
        and re.fullmatch("[a-f0-9]{64}", value["binarySha256"])
        and type(value["binaryBytes"]) is int
        and 0 < value["binaryBytes"] <= MAX_BINARY
    )
    return value


def native() -> NoReturn:
    """Exec the retained authenticated original, without reading either frame."""
    require(os.geteuid() == 0)
    value = config_value(read_original(CONFIG, 4096, 0o600))
    require(read_original(MARKER, 4096, 0o600) == (str(value["runId"]) + "\n").encode())
    descriptor = os.open(BINARY, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        original = os.fstat(descriptor)
        require(
            stat.S_ISREG(original.st_mode)
            and original.st_uid == 0
            and stat.S_IMODE(original.st_mode) == EXECUTABLE_MODE
            and original.st_size == value["binaryBytes"]
            and stamp(BINARY.lstat()) == stamp(original)
        )
        digest = hashlib.sha256()
        total = 0
        while chunk := os.read(descriptor, 65536):
            total += len(chunk)
            require(total <= MAX_BINARY)
            digest.update(chunk)
        require(
            total == value["binaryBytes"]
            and digest.hexdigest() == value["binarySha256"]
            and stamp(os.fstat(descriptor)) == stamp(original)
            and stamp(BINARY.lstat()) == stamp(original)
        )
        os.execve(  # noqa: S606 -- Execute only the retained authenticated original FD.
            "/proc/self/fd/" + str(descriptor),
            [
                "simplestchat-ops",
                "release",
                "receive",
                "--timeout-seconds",
                "300",
                "--finalization-seconds",
                "30",
            ],
            {"PATH": "/usr/bin:/bin", "HOME": "/root", "LC_ALL": "C", "LANG": "C"},
        )
    finally:
        os.close(descriptor)


if __name__ == "__main__":
    try:
        raise SystemExit(native())
    except (OSError, ValueError, TypeError, KeyError, OverflowError):
        _ = sys.stderr.write("Signed receiver fixture route is unproven.\n")
        raise SystemExit(1) from None
