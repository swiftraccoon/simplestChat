"""Read-only release preflight, supplied by the controller, not installed helpers.

An empty hash mapping selects ordinary helper reconciliation. Prepared mode must
supply the exact complete helper set from the controller checkout. No installed
helper is imported, no configuration contents are returned, and no service or
file is changed. This is a snapshot, not a lock spanning the release playbook;
operators must serialize releases and helper updates. Runtime helpers still
take the workload lock and enforce unfinished-operation journals themselves.
"""

# Output is this CLI's public JSON receipt, and exceptions carry fixed codes.
# ruff: noqa: T201, EM101

import hashlib
import json
import os
import platform
import re
import stat
import sys
import uuid
from pathlib import Path
from typing import TypedDict, cast

CONFIG = Path("/etc/simplestchat-public")
ROOT = Path("/srv/simplestchat-public")
HELPERS = Path("/usr/local/libexec/simplestchat-public")
BASE_HELPERS = {
    "release-public.py",
    "release_public.py",
    "release_artifact.py",
    "release_json.py",
    "reboot-public.py",
    "reboot_public.py",
}
FETCH_HELPERS = {"fetch-release.py", "release_fetch_receiver.py"}
MAX_HELPER = 1024 * 1024
MAX_REQUEST = 4096
REQUEST_ARGUMENTS = 2


class PreflightReceipt(TypedDict):
    """Public, non-sensitive proof that one read-only preflight succeeded."""

    schemaVersion: int
    runId: str
    prepared: bool


class PreflightError(Exception):
    """Only fixed, non-sensitive failure codes are printed."""


def require(condition: object, code: str) -> None:
    """Reject an unmet precondition using a fixed, non-sensitive error code."""
    if not condition:
        raise PreflightError(code)


def protected(
    path: Path, *, directory: bool = False, mode: int = 0o600, limit: int | None = None
) -> None:
    """Require root ownership, exact permissions, file kind and bounded size."""
    value = path.lstat()
    require(
        value.st_uid == 0
        and value.st_gid == 0
        and stat.S_IMODE(value.st_mode) == mode
        and (stat.S_ISDIR(value.st_mode) if directory else stat.S_ISREG(value.st_mode)),
        "unsafe_release_path",
    )
    if limit is not None:
        require(0 < value.st_size <= limit, "invalid_release_file_size")


def _expected_helpers(value: object) -> dict[str, str]:
    if not isinstance(value, dict):
        raise PreflightError("invalid_helper_set")
    # This standalone controller-supplied preflight must not import host helpers
    # before their identities have been checked. Validate its tiny input locally.
    raw = cast("dict[object, object]", value)
    require(set(raw) in (set(), BASE_HELPERS, BASE_HELPERS | FETCH_HELPERS), "invalid_helper_set")
    result: dict[str, str] = {}
    for name, digest in raw.items():
        if (
            not isinstance(name, str)
            or not isinstance(digest, str)
            or not re.fullmatch(r"[a-f0-9]{64}", digest)
        ):
            raise PreflightError("invalid_helper_digest")
        result[name] = digest
    return result


def check(value: object) -> PreflightReceipt:
    """Validate host and optional helper identities without executing their code."""
    expected = _expected_helpers(value)
    require(os.geteuid() == 0, "root_required")
    require(platform.system() == "Linux" and platform.machine() == "x86_64", "unsupported_platform")
    release = platform.freedesktop_os_release()
    require(
        release.get("ID") == "debian" and release.get("VERSION_ID", "").split(".")[0] == "13",
        "unsupported_distribution",
    )
    protected(CONFIG, directory=True, mode=0o700)
    protected(ROOT, directory=True, mode=0o700)
    protected(CONFIG / "images.json", limit=65536)
    if expected:
        protected(ROOT / "releases", directory=True, mode=0o700)
        # Reject intermediate directory symlinks and writable code directories.
        for parent in reversed((HELPERS, *HELPERS.parents)):
            protected(parent, directory=True, mode=0o755)
        for name, digest in expected.items():
            source = HELPERS / name
            protected(source, mode=0o644, limit=MAX_HELPER)
            with source.open("rb") as stream:
                actual = hashlib.sha256(stream.read(MAX_HELPER + 1)).hexdigest()
            require(actual == digest, "helper_digest_mismatch")
    return {"schemaVersion": 1, "runId": uuid.uuid4().hex, "prepared": bool(expected)}


def main() -> int:
    """Emit one sanitized receipt, with no configuration or filesystem contents."""
    try:
        require(
            len(sys.argv) == REQUEST_ARGUMENTS and len(sys.argv[1]) <= MAX_REQUEST,
            "invalid_request",
        )
        result = check(cast("object", json.loads(sys.argv[1])))
    except (PreflightError, OSError, ValueError) as error:
        failure = str(error) if isinstance(error, PreflightError) else "preflight_unavailable"
        print(json.dumps({"schemaVersion": 1, "passed": False, "failure": failure}))
        return 1
    print(json.dumps(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
