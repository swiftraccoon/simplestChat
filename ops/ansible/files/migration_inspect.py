"""Read-only machine and fresh-destination checks before migration preparation."""

# This standalone preflight cannot trust installed helper modules yet.
# ruff: noqa: T201

import json
import os
import platform
import re
import shutil
import stat
import subprocess
import sys
from pathlib import Path
from typing import cast
from urllib.parse import urlsplit

CONFIG = Path("/etc/simplestchat-public")
ROOT = Path("/srv/simplestchat-public")
MACHINE = Path("/etc/machine-id")
MAX_CONFIGURATION = 65536
ENV_FIELDS = 2
POSTGRES_UID = 999
ARGUMENT_COUNT = 3


class InspectionError(Exception):
    """Fixed public error codes; never include configuration contents."""


def require(condition: object, code: str) -> None:
    """Refuse a migration precondition without changing either host."""
    if not condition:
        raise InspectionError(code)


def protected(path: Path, *, directory: bool = False) -> None:
    """Require a root-owned private regular file or directory."""
    metadata = path.lstat()
    require(
        metadata.st_uid == 0
        and stat.S_IMODE(metadata.st_mode) == (0o700 if directory else 0o600)
        and (stat.S_ISDIR(metadata.st_mode) if directory else stat.S_ISREG(metadata.st_mode)),
        "unsafe_migration_configuration",
    )
    if not directory:
        require(0 < metadata.st_size <= MAX_CONFIGURATION, "invalid_configuration_size")


def configured(origin: str, *, allow_unconfigured: bool = False) -> dict[str, str | None]:
    """Read only domain, RP ID and source revision from protected configuration."""
    if not CONFIG.exists():
        require(not CONFIG.is_symlink(), "unsafe_migration_configuration")
        return {"domain": None, "rpId": None, "revision": None}
    protected(CONFIG, directory=True)
    environment = CONFIG / "app.env"
    manifest = CONFIG / "images.json"
    present = [path.exists() or path.is_symlink() for path in (environment, manifest)]
    if allow_unconfigured and not any(present):
        if ROOT.exists() or ROOT.is_symlink():
            protected(ROOT, directory=True)
        secrets = CONFIG / "secrets.json"
        if secrets.exists() or secrets.is_symlink():
            protected(secrets)
        return {"domain": None, "rpId": None, "revision": None}
    require(all(present), "configuration_pair_incomplete")
    protected(ROOT, directory=True)
    protected(environment)
    protected(manifest)
    pairs = [
        line.split("=", 1)
        for line in environment.read_text().splitlines()
        if line and not line.startswith("#")
    ]
    require(all(len(pair) == ENV_FIELDS for pair in pairs), "invalid_environment")
    values = dict(pairs)
    require(len(values) == len(pairs), "duplicate_environment_key")
    require(values.get("ALLOWED_ORIGINS") == origin, "source_or_target_origin_mismatch")
    rp_id = values.get("WEBAUTHN_RP_ID", "")
    require(re.fullmatch(r"[a-z0-9.-]{1,253}", rp_id), "invalid_source_rp_id")
    raw = cast("object", json.loads(manifest.read_text()))
    require(isinstance(raw, dict), "invalid_image_manifest")
    record = cast("dict[str, object]", raw)
    revision = record.get("revision")
    require(
        isinstance(revision, str) and re.fullmatch(r"[a-f0-9]{40}", revision),
        "invalid_source_revision",
    )
    return {"domain": urlsplit(origin).hostname, "rpId": rp_id, "revision": cast("str", revision)}


def fresh_target() -> None:
    """Refuse existing application containers or any destination database files."""
    database = ROOT / "postgres"
    if database.exists() or database.is_symlink():
        metadata = database.lstat()
        require(
            stat.S_ISDIR(metadata.st_mode) and metadata.st_uid == POSTGRES_UID,
            "unsafe_target_database",
        )
        require(not any(database.iterdir()), "destination_database_not_empty")
    if shutil.which("docker") is None:
        return
    result = subprocess.run(
        [
            "/usr/bin/docker",
            "--host",
            "unix:///var/run/docker.sock",
            "ps",
            "--all",
            "--quiet",
            "--filter",
            "label=com.docker.compose.project=simplestchat-public",
        ],
        capture_output=True,
        text=True,
        timeout=20,
        check=False,
    )
    require(
        result.returncode == 0 and not result.stdout.strip(), "destination_has_public_containers"
    )


def inspect(role: str, origin: str) -> dict[str, str | bool | None]:
    """Identify the physical host and enforce the supported migration boundary."""
    require(os.geteuid() == 0, "root_required")
    require(role in ("source", "target"), "invalid_migration_role")
    require(re.fullmatch(r"https://[a-z0-9.-]+", origin), "invalid_migration_origin")
    require(platform.system() == "Linux" and platform.machine() == "x86_64", "unsupported_platform")
    release = platform.freedesktop_os_release()
    require(
        release.get("ID") == "debian" and release.get("VERSION_ID") == "13",
        "unsupported_distribution",
    )
    machine = MACHINE.read_text().strip()
    require(re.fullmatch(r"[a-f0-9]{32}", machine), "invalid_machine_identity")
    settings = configured(origin, allow_unconfigured=role == "target")
    if role == "source":
        require(settings["domain"] is not None, "source_not_configured")
    else:
        fresh_target()
    return {"passed": True, "machineId": machine, **settings}


def main() -> int:
    """Print only the inspected identities or a fixed failure code."""
    try:
        require(len(sys.argv) == ARGUMENT_COUNT, "invalid_arguments")
        result = inspect(sys.argv[1], sys.argv[2])
    except (InspectionError, OSError, ValueError, subprocess.SubprocessError) as error:
        print(
            json.dumps(
                {
                    "passed": False,
                    "failure": str(error)
                    if isinstance(error, InspectionError)
                    else "inspection_failed",
                }
            )
        )
        return 1
    print(json.dumps(result))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
