"""Verify one owned release backup in an isolated disposable PostgreSQL container."""

import argparse
import hashlib
import os
import re
import signal
import stat
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path
from types import FrameType
from typing import NoReturn

import release_public as release
from release_artifact import validate_manifest
from release_json import JsonObject, decode_json, object_value, string_value

STATE = Path("/var/lib/simplestchat-monitoring")
VERIFY_SQL = Path("/usr/local/libexec/simplestchat-public/restore-verify.sql")
MAX_BACKUP = 64 * 1024 * 1024
PRIVATE_MODE = 0o600
LABEL = "clinic.research.simplestchat.restore"
SOCKET = "/var/run/postgresql"
DATABASE = "restorecheck"
ROLE_SQL = b"""CREATE ROLE simplestchat_app NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE;
CREATE ROLE simplestchat_migrate NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE;
"""


class Arguments(argparse.Namespace):
    """Expose only the allowlisted release evidence directory name."""

    release_attempt: str = ""


def snapshot(source: Path, target: Path, expected: str) -> None:
    """Copy one bounded no-follow archive and verify the bytes that will actually restore."""
    descriptor = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as incoming, target.open("xb") as outgoing:
        info = os.fstat(incoming.fileno())
        release.require(
            stat.S_ISREG(info.st_mode)
            and info.st_uid == 0
            and stat.S_IMODE(info.st_mode) == PRIVATE_MODE
            and 0 < info.st_size <= MAX_BACKUP,
            "Unsafe or oversized release backup",
        )
        digest = hashlib.sha256()
        size = 0
        while chunk := incoming.read(min(1024 * 1024, MAX_BACKUP + 1 - size)):
            size += len(chunk)
            release.require(size <= MAX_BACKUP, "Release backup exceeded its bound")
            digest.update(chunk)
            _ = outgoing.write(chunk)
        outgoing.flush()
        os.fsync(outgoing.fileno())
        release.require(
            size == info.st_size and digest.hexdigest() == expected, "Release backup digest differs"
        )


def create_arguments(name: str, image: str) -> list[str]:
    """Use only disposable memory filesystems and the already present production image."""
    return [
        "create",
        "--name",
        name,
        "--label",
        f"{LABEL}={name}",
        "--pull",
        "never",
        "--network",
        "none",
        "--read-only",
        "--user",
        "999:999",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges:true",
        "--memory",
        "512m",
        "--memory-swap",
        "512m",
        "--cpus",
        "0.5",
        "--pids-limit",
        "64",
        "--shm-size",
        "16m",
        "--log-driver",
        "none",
        "--tmpfs",
        "/var/lib/postgresql:rw,nosuid,nodev,noexec,size=256m,uid=999,gid=999,mode=0700",
        "--tmpfs",
        "/var/run/postgresql:rw,nosuid,nodev,noexec,size=1m,uid=999,gid=999,mode=0700",
        "--tmpfs",
        "/tmp:rw,nosuid,nodev,noexec,size=16m,mode=1777",  # noqa: S108 - private container tmpfs.
        "--env",
        "PGDATA=/var/lib/postgresql/data",
        "--env",
        f"POSTGRES_DB={DATABASE}",
        "--env",
        "POSTGRES_HOST_AUTH_METHOD=trust",
        "--env",
        "POSTGRES_INITDB_ARGS=--auth-local=peer --auth-host=reject",
        "--entrypoint",
        "docker-entrypoint.sh",
        image,
        "postgres",
        "-c",
        "listen_addresses=",
        "-c",
        f"unix_socket_directories={SOCKET}",
        "-c",
        "shared_buffers=32MB",
        "-c",
        "work_mem=4MB",
        "-c",
        "maintenance_work_mem=32MB",
        "-c",
        "max_connections=10",
        "-c",
        "statement_timeout=120000",
    ]


def sql(runner: release.RunnerProtocol, container: str, source: bytes) -> bytes:
    """Run fixed SQL only through the disposable container's own private socket."""
    return runner.docker(
        "exec",
        "-i",
        "--user",
        "999:999",
        container,
        "psql",
        "-X",
        "-q",
        "-t",
        "-A",
        "--set",
        "ON_ERROR_STOP=1",
        "-h",
        SOCKET,
        "-U",
        "postgres",
        "-d",
        DATABASE,
        input_data=source,
        timeout=45,
    )


def cleanup(runner: release.RunnerProtocol, name: str) -> None:
    """Remove only the unique labeled container, including one created before a timeout."""
    selectors = (
        "ps",
        "--all",
        "--quiet",
        "--no-trunc",
        "--filter",
        f"label={LABEL}={name}",
        "--filter",
        f"name=^/{name}$",
    )
    identity = runner.docker(*selectors).decode().strip()
    if identity:
        release.require(re.fullmatch("[a-f0-9]{64}", identity), "Ambiguous restore container")
        _ = runner.docker("rm", "--force", "--volumes", identity, timeout=30)
    release.require(not runner.docker(*selectors).strip(), "Restore container cleanup incomplete")


def verify(
    runner: release.RunnerProtocol, image: str, backup: Path, migrations: dict[str, str]
) -> JsonObject:
    """Restore all archive sections and validate before destroying the owned database."""
    name = "scpub-restore-" + uuid.uuid4().hex
    report: JsonObject = {"container": name, "cleanupPassed": False, "verified": False}
    release.atomic(runner.attempt / "restore.json", report)
    release.require(
        not runner.docker("ps", "--all", "--quiet", "--filter", f"label={LABEL}").strip(),
        "Inspect the retained restore container before starting another",
    )
    try:
        identity = runner.docker(*create_arguments(name, image), timeout=30).decode().strip()
        release.require(
            re.fullmatch("[a-f0-9]{64}", identity), "Invalid restore container identity"
        )
        _ = runner.docker("start", identity)
        deadline = time.monotonic() + 30
        while True:
            try:
                _ = runner.docker(
                    "exec",
                    "--user",
                    "999:999",
                    identity,
                    "/bin/sh",
                    "-c",
                    'test "$(cat /proc/1/comm)" = postgres && '
                    + f"pg_isready -q -h {SOCKET} -U postgres -d {DATABASE}",
                    timeout=3,
                )
                break
            except (release.ReleaseError, subprocess.TimeoutExpired):
                release.require(time.monotonic() < deadline, "Isolated PostgreSQL was not ready")
                time.sleep(0.5)
        _ = sql(runner, identity, ROLE_SQL)
        _ = runner.docker(
            "exec",
            "-i",
            "--user",
            "999:999",
            identity,
            "timeout",
            "--signal=TERM",
            "--kill-after=2s",
            "120s",
            "pg_restore",
            "--exit-on-error",
            "--single-transaction",
            "-h",
            SOCKET,
            "-U",
            "postgres",
            "-d",
            DATABASE,
            input_path=backup,
            timeout=125,
        )
        expected = "\n".join(
            f"{version}|t|{checksum}"
            for version, checksum in sorted(migrations.items(), key=lambda item: int(item[0]))
        )
        actual = sql(runner, identity, release.LEDGER_QUERY.encode()).decode().strip()
        release.require(actual == expected, "Restored migration ledger differs from release")
        report["counts"] = object_value(decode_json(sql(runner, identity, VERIFY_SQL.read_bytes())))
        _ = runner.docker(
            "exec",
            "--user",
            "999:999",
            identity,
            "timeout",
            "--signal=TERM",
            "--kill-after=2s",
            "120s",
            "pg_amcheck",
            "--install-missing",
            "--heapallindexed",
            "--parent-check",
            "-h",
            SOCKET,
            "-U",
            "postgres",
            "-d",
            DATABASE,
            timeout=125,
        )
        report["verified"] = True
    finally:
        try:
            cleanup(runner, name)
            report["cleanupPassed"] = True
        finally:
            release.atomic(runner.attempt / "restore.json", report)
    return report


def main() -> None:
    """Accept only recorded owned release backups and publish success after cleanup."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument(
        "release_attempt", help="release.NAME beneath the private results directory"
    )
    arguments = parser.parse_args(namespace=Arguments())
    release.require(os.geteuid() == 0, "Restore verification requires root")
    release.require(
        re.fullmatch(r"release\.[A-Za-z0-9_-]{6,64}", arguments.release_attempt),
        "Select one owned release attempt",
    )
    _ = os.umask(0o077)

    def interrupted(_signum: int, _frame: FrameType | None) -> NoReturn:
        raise release.ReleaseError("Restore verification interrupted")  # noqa: EM101, TRY003

    for signum in (signal.SIGTERM, signal.SIGINT):
        _ = signal.signal(signum, interrupted)
    with release.workload_lock():
        release.protected(STATE, directory=True, modes=(0o750,))
        for directory in (release.ROOT / "results", release.ROOT / "releases"):
            release.protected(directory, directory=True, modes=(0o700,))
        source = release.ROOT / "results" / arguments.release_attempt
        release.protected(source, directory=True, modes=(0o700,))
        release.protected(source / "outcome.json", limit=65536)
        outcome = object_value(decode_json((source / "outcome.json").read_bytes()))
        revision = string_value(outcome["revision"])
        digest = string_value(outcome["backupSha256"])
        release.require(
            re.fullmatch("[a-f0-9]{40}", revision) and re.fullmatch("[a-f0-9]{64}", digest),
            "Invalid backup identity",
        )
        manifest_dir = release.ROOT / "releases" / revision
        release.protected(manifest_dir, directory=True, modes=(0o700,))
        release.protected(manifest_dir / "release.json", limit=1024 * 1024)
        manifest = validate_manifest(manifest_dir / "release.json")
        release.require(manifest["revision"] == revision, "Backup release identity differs")
        attempts = STATE / "restores"
        attempts.mkdir(mode=0o700, exist_ok=True)
        release.protected(attempts, directory=True, modes=(0o700,))
        attempt = Path(tempfile.mkdtemp(prefix="restore.", dir=attempts))
        runner = release.Runner(attempt)
        backup = attempt / "snapshot.dump"
        try:
            snapshot(source / "database-before.dump", backup, digest)
            image = (
                runner.docker("inspect", "--format", "{{.Image}}", "simplestchat-public-postgres-1")
                .decode()
                .strip()
            )
            release.require(release.ID.fullmatch(image), "Invalid production PostgreSQL image")
            report = verify(runner, image, backup, manifest["migrations"])
        finally:
            backup.unlink(missing_ok=True)
        release.require(
            report.get("verified") is True and report.get("cleanupPassed") is True,
            "Restore verification or cleanup was not successful",
        )
        report.update(
            {"backupSha256": digest, "release": revision, "finishedAt": release.timestamp()}
        )
        release.atomic(attempt / "outcome.json", report)
        release.atomic(STATE / "restore-verified.timestamp", {"evidence": str(attempt), **report})
        _ = sys.stdout.write("Isolated restore verified and disposable database removed.\n")


def cli() -> int:
    """Keep private restore errors and source data out of terminal output."""
    try:
        main()
    except (release.ReleaseError, OSError, ValueError, KeyError, subprocess.SubprocessError):
        _ = sys.stderr.write("Restore verification failed; inspect private restore evidence.\n")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(cli())
