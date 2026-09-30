"""Disable PostgreSQL bound-parameter logging without restarting public services.

Only the two fixed pg_settings entries are read. The operation holds the shared
workload lock, addresses the current database by full container ID, and records
private evidence. Separate ALTER SYSTEM statements are safe to repeat after an
interruption; successful reload is verified through new database connections.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import TYPE_CHECKING, NoReturn, Protocol, Unpack

import release_public as release
from release_json import JsonObject, array_value, decode_json, object_value, string_value

if TYPE_CHECKING:
    from collections.abc import Sequence
    from types import FrameType

    from release_public import CommandOptions

SETTINGS = {
    "log_parameter_max_length": "superuser",
    "log_parameter_max_length_on_error": "user",
}
QUERY = (
    "SELECT json_agg(row_to_json(s) ORDER BY name) FROM ("
    "SELECT name, setting, source, context, pending_restart FROM pg_settings "
    "WHERE name IN ('log_parameter_max_length','log_parameter_max_length_on_error')"
    ") AS s"
)
ALTER = tuple(f"ALTER SYSTEM SET {name} = '0'" for name in SETTINGS)
RELOAD = "SELECT pg_reload_conf()"
SQL = frozenset((QUERY, RELOAD, *ALTER))
VERIFY_SECONDS = 15
VERIFY_POLL_SECONDS = 0.25


class DatabaseRunner(Protocol):
    """Only current container inspection and bounded fixed Docker commands are needed."""

    def container(self, service: str) -> JsonObject:
        """Inspect one current running Compose service."""
        ...

    def docker(self, *args: str, **kwargs: Unpack[CommandOptions]) -> bytes:
        """Execute fixed local Docker argv and retain bounded private output."""
        ...


def query(runner: DatabaseRunner, database: str, statement: str) -> bytes:
    """Use a fresh noninteractive socket session for an allowlisted statement."""
    release.require(re.fullmatch(r"[a-f0-9]{64}", database), "Invalid database container identity")
    release.require(statement in SQL, "Unsupported database logging statement")
    return runner.docker(
        "exec",
        "--user",
        "999:999",
        "--env",
        "PGOPTIONS=-c statement_timeout=5000 -c lock_timeout=3000",
        database,
        "psql",
        "--no-psqlrc",
        "--no-password",
        "--set",
        "ON_ERROR_STOP=on",
        "--host",
        "/run/simplestchat-postgres",
        "--username",
        "postgres",
        "--dbname",
        "simplestchat",
        "--tuples-only",
        "--no-align",
        "--command",
        statement,
        timeout=10,
    )


def settings(raw: bytes) -> JsonObject:
    """Validate exactly the two reloadable settings and reject overriding session policy."""
    rows = array_value(decode_json(raw))
    release.require(len(rows) == len(SETTINGS), "Missing database logging settings")
    result: JsonObject = {}
    for value in rows:
        row = object_value(value)
        name = string_value(row.get("name"))
        setting = string_value(row.get("setting"))
        release.require(name in SETTINGS and name not in result, "Unexpected logging setting")
        release.require(re.fullmatch(r"-1|[0-9]{1,10}", setting), "Invalid logging setting value")
        release.require(row.get("context") == SETTINGS[name], "Unexpected logging setting context")
        release.require(row.get("pending_restart") is False, "Database setting has pending restart")
        source = row.get("source")
        release.require(
            source in ("default", "configuration file", "command line"),
            "Database logging has an overriding session, role or database policy",
        )
        release.require(
            source != "command line" or setting == "0",
            "Contradictory command-line logging setting requires full configuration maintenance",
        )
        result[name] = row
    return result


def explicit_zero(snapshot: JsonObject) -> bool:
    """Recognize persisted safe values, excluding zeros inherited only from defaults."""
    return all(
        object_value(snapshot[name]).get("setting") == "0"
        and object_value(snapshot[name]).get("source") in ("configuration file", "command line")
        for name in SETTINGS
    )


def unchanged(runner: DatabaseRunner, original: JsonObject) -> None:
    """Require the same current database identity, image, start time and restart count."""
    release.require(
        release.stable_container(runner.container("postgres"))
        == release.stable_container(original),
        "Database container changed during logging remediation",
    )


def apply(runner: DatabaseRunner, report: JsonObject) -> None:
    """Apply only fixed zero values and verify the effective policy after asynchronous reload."""
    original = runner.container("postgres")
    database = string_value(original.get("id"))
    report["databaseContainer"] = database
    before = settings(query(runner, database, QUERY))
    report["before"] = before
    report["changed"] = False
    if explicit_zero(before):
        unchanged(runner, original)
        report["after"] = before
        return
    for statement in ALTER:
        unchanged(runner, original)
        report["changed"] = True
        _ = query(runner, database, statement)
    unchanged(runner, original)
    release.require(query(runner, database, RELOAD).strip() == b"t", "Database reload was refused")
    deadline = time.monotonic() + VERIFY_SECONDS
    while True:
        after = settings(query(runner, database, QUERY))
        report["after"] = after
        if explicit_zero(after):
            unchanged(runner, original)
            return
        release.require(time.monotonic() < deadline, "Database logging reload was not effective")
        time.sleep(VERIFY_POLL_SECONDS)


def execute() -> JsonObject:
    """Hold the canonical lock and preserve a private outcome on success or partial failure."""
    release.require(os.geteuid() == 0, "Run as root on the prepared public host")
    _ = os.umask(0o077)
    with release.workload_lock():
        parent = release.ROOT / "results"
        release.protected(parent, directory=True, modes=(0o700,))
        attempt = Path(tempfile.mkdtemp(prefix="database-logging.", dir=parent))
        report: JsonObject = {
            "schemaVersion": 1,
            "operation": "database_logging",
            "evidence": str(attempt),
            "startedAt": release.timestamp(),
            "passed": False,
            "changed": False,
            "verificationConnection": {"role": "postgres", "database": "simplestchat"},
        }
        try:
            provenance: JsonObject = {}
            for name in ("database_logging.py", "release_public.py", "bounded_process.py"):
                with Path(__file__).with_name(name).open("rb") as source:
                    provenance[name] = hashlib.file_digest(source, "sha256").hexdigest()
            report["sourceSha256"] = provenance
            apply(release.Runner(attempt), report)
            report["passed"] = True
        except BaseException as error:
            report["failure"] = (
                str(error) if isinstance(error, release.ReleaseError) else type(error).__name__
            )
            raise
        finally:
            report["finishedAt"] = release.timestamp()
            release.atomic(attempt / "outcome.json", report)
            _ = sys.stdout.write(json.dumps(report) + "\n")
        return report


def main(argv: Sequence[str] | None = None) -> int:
    """Expose one fixed operation, with no host, SQL or configuration-value arguments."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("action", choices=("apply",))
    _ = parser.parse_args(argv)

    def interrupted(_signum: int, _frame: FrameType | None) -> NoReturn:
        raise release.ReleaseError("Database logging remediation interrupted")  # noqa: EM101, TRY003 -- fixed diagnostic.

    for signum in (signal.SIGTERM, signal.SIGINT):
        _ = signal.signal(signum, interrupted)
    try:
        _ = execute()
    except (
        release.ReleaseError,
        OSError,
        ValueError,
        KeyError,
        subprocess.SubprocessError,
    ) as error:
        _ = sys.stderr.write(
            f"Database logging remediation failed ({type(error).__name__}); "
            + "inspect private evidence.\n"
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
