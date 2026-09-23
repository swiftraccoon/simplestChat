"""Back up PostgreSQL before creating its separately owned operational schema."""

import os
import subprocess
import sys
import time
from pathlib import Path

from monitoring_collect import docker
from release_public import require

PREFIX = [
    "/usr/bin/docker",
    "--host",
    "unix:///var/run/docker.sock",
    "exec",
    "-i",
    "--user",
    "postgres",
    "simplestchat-public-postgres-1",
]
CONNECTION = ["-h", "/run/simplestchat-postgres", "-U", "postgres", "-d", "simplestchat"]


def main() -> None:
    """Refuse unknown schemas and retain one private verified archive before initial creation."""
    require(os.geteuid() == 0, "Schema preparation requires root")
    _ = os.umask(0o077)
    existing = docker(
        "exec",
        "--user",
        "postgres",
        "simplestchat-public-postgres-1",
        "psql",
        "-X",
        "-q",
        "-t",
        "-A",
        *CONNECTION,
        "-c",
        "SELECT to_regclass('operations.schema_version');",
    )
    if existing:
        version = docker(
            "exec",
            "--user",
            "postgres",
            "simplestchat-public-postgres-1",
            "psql",
            "-X",
            "-q",
            "-t",
            "-A",
            *CONNECTION,
            "-c",
            "SELECT version FROM operations.schema_version;",
        )
        require(version == "1", "Unsupported operations schema version")
        # Check required columns and singleton shape before declaring it retained.
        _ = docker(
            "exec",
            "--user",
            "postgres",
            "simplestchat-public-postgres-1",
            "psql",
            "-X",
            "-q",
            "-t",
            "-A",
            *CONNECTION,
            "--set",
            "ON_ERROR_STOP=1",
            "-c",
            """SELECT incident_key,rule,severity,resource,first_seen,last_seen,resolved_at,
            release_revision,observations FROM operations.alerts LIMIT 0;
            SELECT singleton,observed_at FROM operations.alert_cursor LIMIT 0;""",
        )
        permissions = docker(
            "exec",
            "--user",
            "postgres",
            "simplestchat-public-postgres-1",
            "psql",
            "-X",
            "-q",
            "-t",
            "-A",
            *CONNECTION,
            "-c",
            """SELECT nspowner=(SELECT oid FROM pg_roles WHERE rolname='postgres')
            AND NOT has_schema_privilege('simplestchat_app','operations','USAGE')
            AND NOT has_schema_privilege('simplestchat_migrate','operations','USAGE')
            FROM pg_namespace WHERE nspname='operations';""",
        )
        require(permissions == "t", "Operations ownership or role privileges changed")
        _ = sys.stdout.write("retained\n")
        return
    backup = Path("/var/lib/simplestchat-monitoring") / f"schema-before-{time.time_ns()}.dump"
    with backup.open("xb") as output:
        _ = subprocess.run(  # noqa: S603 - fixed local daemon and pg_dump arguments.
            [*PREFIX, "pg_dump", "--format=custom", *CONNECTION],
            stdout=output,
            stderr=subprocess.PIPE,
            timeout=60,
            check=True,
        )
        output.flush()
        os.fsync(output.fileno())
    require(backup.stat().st_size > 0, "Empty schema backup")
    with backup.open("rb") as source:
        _ = subprocess.run(  # noqa: S603 - inspect only the archive from the owned database.
            [*PREFIX, "pg_restore", "--list"],
            stdin=source,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            check=True,
            timeout=30,
        )
    sql = Path("/usr/local/libexec/simplestchat-public/monitoring-schema.sql").read_bytes()
    _ = subprocess.run(  # noqa: S603 - fixed reviewed SQL, never user-supplied statements.
        [*PREFIX, "psql", "-X", "-q", *CONNECTION],
        input=sql,
        capture_output=True,
        timeout=15,
        check=True,
    )
    _ = sys.stdout.write("created\n")


if __name__ == "__main__":
    main()
