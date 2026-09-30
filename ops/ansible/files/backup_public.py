"""Create private durable nightly archives under the canonical public workload lock."""

import argparse
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from datetime import UTC, datetime
from pathlib import Path
from types import FrameType
from typing import TYPE_CHECKING, NoReturn

import release_public as release
from release_artifact import sha256_file

if TYPE_CHECKING:
    from release_json import JsonObject

TARGET = release.ROOT / "backups" / "nightly"
STAMP = re.compile(r"[0-9]{8}T[0-9]{6}Z")
MAX_KEEP_DAYS = 3650
MAX_KEEP_COUNT = 365
DAEMON_CLEANUP_SECONDS = 480


class Arguments(argparse.Namespace):
    """Accept bounded retention settings and a cleanup-only wrapper action."""

    action: str = "nightly"
    keep_days: int = 14
    keep_at_least: int = 3


def retention(keep_days: int, keep_at_least: int) -> None:
    """Reject booleans, negative, zero and excessive retention limits before any write."""
    release.require(
        type(keep_days) is int and 1 <= keep_days <= MAX_KEEP_DAYS,
        "Backup retention days must be an integer between 1 and 3650",
    )
    release.require(
        type(keep_at_least) is int and 1 <= keep_at_least <= MAX_KEEP_COUNT,
        "Backup minimum count must be an integer between 1 and 365",
    )


def cleanup_partials(target: Path) -> None:
    """Remove only recognized private orphan partials while holding the shared lock."""
    release.protected(target, directory=True, modes=(0o700,))
    for path in target.glob("*.partial"):
        if re.fullmatch(r"[0-9]{8}T[0-9]{6}Z\.(?:dump|receipt\.json)\.partial", path.name):
            release.protected(path)
            path.unlink()


def prune(target: Path, keep_days: int, keep_at_least: int) -> None:
    """Retain the newest valid archives; never follow links or prune release evidence."""
    retention(keep_days, keep_at_least)
    records: list[tuple[float, Path, Path]] = []
    for receipt in target.glob("*.receipt.json"):
        dump, _record, completed = release.validated_backup(receipt)
        records.append((completed, dump, receipt))
    boundary = time.time() - keep_days * 86400
    for completed, dump, receipt in sorted(records, reverse=True)[keep_at_least:]:
        if completed < boundary:
            receipt.unlink()
            dump.unlink()
    # Command output is separate from archive evidence and bounded by the same age policy.
    for directory in target.glob("*.logs"):
        if STAMP.fullmatch(directory.name.removesuffix(".logs")):
            release.protected(directory, directory=True, modes=(0o700,))
            if directory.stat().st_mtime < boundary:
                shutil.rmtree(directory)


def dump_database(runner: release.RunnerProtocol, database: str, partial: Path) -> None:
    """Bound both daemon-side dump lifetime and controller wait; list before publication."""
    _ = runner.docker(
        "exec",
        "--user",
        "999:999",
        database,
        "timeout",
        "--signal=TERM",
        "--kill-after=2s",
        "120s",
        "pg_dump",
        "--host",
        "/run/simplestchat-postgres",
        "--username",
        "postgres",
        "--dbname",
        "simplestchat",
        "--format",
        "custom",
        output_path=partial,
        timeout=130,
    )
    release.require(partial.stat().st_size > 0, "Empty nightly backup")
    _ = runner.docker(
        "exec",
        "--interactive",
        "--user",
        "999:999",
        database,
        "timeout",
        "--signal=TERM",
        "--kill-after=2s",
        "30s",
        "pg_restore",
        "--list",
        input_path=partial,
        timeout=40,
    )


def nightly(target: Path, keep_days: int, keep_at_least: int) -> Path:
    """Publish archive and receipt only after validation, then apply bounded retention."""
    retention(keep_days, keep_at_least)
    cleanup_partials(target)
    stamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")
    logs = target / f"{stamp}.logs"
    logs.mkdir(mode=0o700)
    runner = release.Runner(logs)
    database = (
        runner.docker(
            "ps", "--quiet", "--no-trunc", "--filter", "name=^simplestchat-public-postgres-1$"
        )
        .decode()
        .strip()
    )
    release.require(re.fullmatch(r"[a-f0-9]{64}", database), "Public database is not running")
    release.backup_headroom(runner, database)
    migrations = release.ledger(runner, database)
    image = runner.docker("inspect", "--format", "{{.Image}}", database).decode().strip()
    release.require(release.ID.fullmatch(image), "Invalid database image identity")
    partial, dump = target / f"{stamp}.dump.partial", target / f"{stamp}.dump"
    try:
        dump_database(runner, database, partial)
        release.publish_backup(partial, dump)
        receipt: JsonObject = {
            "schemaVersion": 1,
            "dump": dump.name,
            "bytes": dump.stat().st_size,
            "sha256": sha256_file(dump),
            "completedAt": release.timestamp(),
            "postgresImage": image,
            "backupMigrations": dict(migrations),
        }
        release.atomic(target / f"{stamp}.receipt.json", receipt)
    finally:
        partial.unlink(missing_ok=True)
    prune(target, keep_days, keep_at_least)
    return dump


def main() -> None:
    """Refuse unsafe paths and busy/unfinished work before any backup or orphan cleanup."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("action", choices=("nightly", "cleanup"))
    _ = parser.add_argument("--keep-days", type=int, default=14)
    _ = parser.add_argument("--keep-at-least", type=int, default=3)
    arguments = parser.parse_args(namespace=Arguments())
    retention(arguments.keep_days, arguments.keep_at_least)
    release.require(os.geteuid() == 0, "Nightly backup requires root")
    _ = os.umask(0o077)

    def interrupted(_signum: int, _frame: FrameType | None) -> NoReturn:
        raise release.ReleaseError("Nightly backup interrupted")  # noqa: EM101, TRY003

    for signum in (signal.SIGTERM, signal.SIGINT):
        _ = signal.signal(signum, interrupted)
    with release.workload_lock(recover_expired_backup=True):
        release.protected(TARGET, directory=True, modes=(0o700,))
        cleanup_partials(TARGET)
        current: JsonObject = {
            "schemaVersion": 1,
            "operation": "nightly_backup",
            "bootId": release.boot_id(),
            "untilMonotonic": time.monotonic() + DAEMON_CLEANUP_SECONDS,
            "finalized": False,
        }
        if arguments.action == "cleanup":
            current["finalized"] = True
            release.atomic(release.WORK / "current.json", current)
        else:
            release.atomic(release.WORK / "current.json", current)
            dump = nightly(TARGET, arguments.keep_days, arguments.keep_at_least)
            current["finalized"] = True
            release.atomic(release.WORK / "current.json", current)
            _ = sys.stdout.write(f"Nightly backup {dump.name} validated and recorded.\n")


def cli() -> int:
    """Report fixed failure categories without printing database output or credentials."""
    try:
        main()
    except (OSError, ValueError, KeyError, release.ReleaseError, subprocess.SubprocessError):
        _ = sys.stderr.write(
            "Nightly backup failed; inspect the private backup logs and journals.\n"
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(cli())
