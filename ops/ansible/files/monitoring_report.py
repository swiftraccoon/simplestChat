"""Print a bounded private read-only incident and external-check report."""

import json
import os
import subprocess
import sys
from pathlib import Path

from release_json import JsonObject, decode_json, integer_value, object_value
from release_public import ReleaseError, require

PSQL = [
    "/usr/bin/docker",
    "--host",
    "unix:///var/run/docker.sock",
    "exec",
    "-i",
    "--user",
    "postgres",
    "simplestchat-public-postgres-1",
    "psql",
    "-X",
    "-q",
    "-t",
    "-A",
    "--set",
    "ON_ERROR_STOP=1",
    "-h",
    "/run/simplestchat-postgres",
    "-U",
    "postgres",
    "-d",
    "simplestchat",
]
STATE = Path("/var/lib/simplestchat-monitoring")
SQL_ROOT = Path("/usr/local/libexec/simplestchat-public")


def database(source: bytes, payload: str | None = None) -> bytes:
    """Use fixed local peer authentication and safely quoted JSON for reviewed statements."""
    arguments = [*PSQL]
    if payload is not None:
        arguments += ["--set", "payload=" + payload]
    result = subprocess.run(  # noqa: S603 - fixed local daemon/socket and psql-quoted payload.
        arguments,
        input=source,
        capture_output=True,
        check=True,
        timeout=8,
        env={"PATH": "/usr/bin:/bin", "LC_ALL": "C"},
    )
    require(len(result.stdout) <= 256 * 1024, "Oversized operational database response")
    return result.stdout.strip()


def importer_state() -> JsonObject:
    """Expose local bounded spool loss even when dropped snapshots could not reach PostgreSQL."""
    path = STATE / "external.json"
    state: JsonObject = {}
    if path.exists():
        data = path.read_bytes()
        require(len(data) <= 1024 * 1024, "Oversized external importer state")
        state = object_value(decode_json(data))
    return {
        "lastPollEpoch": state.get("lastPollAt"),
        "spoolSnapshots": len(list((STATE / "external-spool").glob("*.json"))),
        "discardedSnapshots": integer_value(state.get("dropped", 0)),
        "scope": "Local spool health; a discarded snapshot means historical coverage was lost.",
    }


def main() -> int:
    """Expose operational metadata only, without giving application roles schema access."""
    try:
        require(os.geteuid() == 0, "Private report requires root")
        result = database((SQL_ROOT / "monitoring-report.sql").read_bytes())
        report = object_value(decode_json(result))
        report["importer"] = importer_state()
    except (ReleaseError, OSError, ValueError, subprocess.SubprocessError):
        _ = sys.stderr.write("Operational report unavailable; inspect private monitoring health.\n")
        return 1
    _ = sys.stdout.write(json.dumps(report, separators=(",", ":")) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
