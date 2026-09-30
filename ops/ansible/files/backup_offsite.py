"""Upload encrypted nightly archives and restore exact snapshots in isolated PostgreSQL."""

import argparse
import os
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from types import FrameType
from typing import NoReturn

import backup_public as nightly
import release_public as release
import restore_verify as restore
from release_json import JsonObject, decode_json, object_value, string_value

CONFIG = Path("/etc/simplestchat-backup")
STATE = Path("/var/lib/simplestchat-monitoring")
MAX_ARCHIVE_MIB = 4096
MIB = 1024 * 1024
MAX_PORT = 65535
MAX_DOWNLOAD_FILES = 2


@dataclass(frozen=True)
class Settings:
    """A fixed SFTP destination and bounded restore resources; credentials stay in files."""

    host: str
    user: str
    port: int
    repository_path: str
    archive_mib: int
    limits: restore.RestoreLimits

    @property
    def repository(self) -> str:
        """Return a credential-free repository locator."""
        return f"sftp:{self.user}@{self.host}:{self.repository_path}"

    def command(self) -> list[str]:
        """Use dedicated files, strict host keys and no ambient SSH or password environment."""
        ssh = [
            "/usr/bin/ssh",
            "-F",
            "/dev/null",
            "-T",
            "-p",
            str(self.port),
            "-i",
            str(CONFIG / "identity"),
            "-o",
            "BatchMode=yes",
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "IdentityAgent=none",
            "-o",
            "PasswordAuthentication=no",
            "-o",
            "KbdInteractiveAuthentication=no",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            f"UserKnownHostsFile={CONFIG / 'known_hosts'}",
            "-o",
            "GlobalKnownHostsFile=/dev/null",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "PermitLocalCommand=no",
            "-o",
            "ConnectTimeout=15",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
            "-s",
            f"{self.user}@{self.host}",
            "sftp",
        ]
        return [
            "/usr/bin/restic",
            "--no-cache",
            "--repo",
            self.repository,
            "--password-file",
            str(CONFIG / "password"),
            "-o",
            "sftp.command=" + shlex.join(ssh),
        ]


def settings() -> Settings:
    """Require a current strict configuration and private regular credential files."""
    release.protected(CONFIG, directory=True, modes=(0o700,))
    release.protected(CONFIG / "settings.json", limit=16384)
    for name in ("identity", "known_hosts", "password"):
        path = CONFIG / name
        release.protected(path, limit=65536)
        release.require(path.stat().st_size > 0, "Empty offsite credential or host-key file")
    value = object_value(decode_json((CONFIG / "settings.json").read_bytes()))
    release.require(
        set(value)
        == {
            "schemaVersion",
            "host",
            "user",
            "port",
            "repositoryPath",
            "archiveLimitMiB",
            "restoreMemoryMiB",
            "restoreDataMiB",
        }
        and type(value["schemaVersion"]) is int
        and value["schemaVersion"] == 1,
        "Invalid offsite schema",
    )
    host, user, repository = (
        string_value(value[key]) for key in ("host", "user", "repositoryPath")
    )
    release.require(
        re.fullmatch(r"[a-z0-9](?:[a-z0-9.-]{0,251}[a-z0-9])?", host), "Invalid offsite host"
    )
    release.require(re.fullmatch(r"[a-z_][a-z0-9_-]{0,31}", user), "Invalid offsite user")
    release.require(
        re.fullmatch(r"/(?:[A-Za-z0-9_-]+/)*[A-Za-z0-9_-]+", repository),
        "Invalid offsite repository path",
    )
    integers: dict[str, int] = {}
    for key in ("port", "archiveLimitMiB", "restoreMemoryMiB", "restoreDataMiB"):
        item = value[key]
        release.require(type(item) is int, "Offsite resource values must be integers")
        if isinstance(item, int):
            integers[key] = item
    release.require(1 <= integers["port"] <= MAX_PORT, "Invalid offsite port")
    release.require(
        1 <= integers["archiveLimitMiB"] <= MAX_ARCHIVE_MIB, "Invalid restore archive bound"
    )
    limits = restore.RestoreLimits(integers["restoreMemoryMiB"], integers["restoreDataMiB"])
    limits.validate()
    return Settings(host, user, integers["port"], repository, integers["archiveLimitMiB"], limits)


def current_archive() -> tuple[Path, Path, JsonObject]:
    """Select the newest verified current-format nightly archive within a 36-hour window."""
    candidates: list[tuple[float, Path, Path, JsonObject]] = []
    for receipt in nightly.TARGET.glob("*.receipt.json"):
        archive, record, completed = release.validated_backup(receipt)
        candidates.append((completed, archive, receipt, record))
    release.require(candidates, "No verified nightly backup is available")
    completed, archive, receipt, record = max(candidates, key=lambda item: item[0])
    release.require(
        time.time() - completed <= 36 * 3600, "Nightly backup is too old for offsite upload"
    )
    _ = archive_identity(record)
    return archive, receipt, record


def archive_identity(record: JsonObject) -> tuple[str, dict[str, str]]:
    """Require the exact database image and migration ledger recorded with this archive."""
    image = string_value(record.get("postgresImage"))
    release.require(release.ID.fullmatch(image), "Nightly receipt lacks its PostgreSQL image")
    migrations = {
        key: string_value(value)
        for key, value in object_value(record.get("backupMigrations")).items()
    }
    release.require(
        bool(migrations)
        and all(
            re.fullmatch(r"[1-9][0-9]*", version) and re.fullmatch(r"[a-f0-9]{96}", checksum)
            for version, checksum in migrations.items()
        ),
        "Nightly receipt lacks its exact migration ledger",
    )
    return image, migrations


def upload(runner: release.RunnerProtocol, configured: Settings) -> JsonObject:
    """Encrypt a verified archive and receipt, retaining its exact immutable snapshot identity."""
    archive, receipt, record = current_archive()
    output = runner.run(
        [
            *configured.command(),
            "backup",
            "--json",
            "--tag",
            "simplestchat-nightly",
            "--",
            str(archive),
            str(receipt),
        ],
        timeout=900,
    )
    summaries = [object_value(decode_json(line)) for line in output.splitlines() if line.strip()]
    empty: JsonObject = {}
    summary = next(
        (item for item in reversed(summaries) if item.get("message_type") == "summary"), empty
    )
    snapshot = string_value(summary.get("snapshot_id"))
    release.require(
        re.fullmatch(r"[a-f0-9]{64}", snapshot), "Missing exact uploaded snapshot identity"
    )
    result: JsonObject = {
        "schemaVersion": 1,
        "repository": configured.repository,
        "snapshot": snapshot,
        "stamp": archive.stem,
        "sha256": record["sha256"],
        "finishedAt": release.timestamp(),
    }
    release.atomic(STATE / "offsite-latest.json", result)
    release.atomic(runner.attempt / "outcome.json", result)
    return result


def selection(configured: Settings, snapshot: str, stamp: str) -> tuple[str, str]:
    """Use an explicit exact snapshot or the last confirmed upload from this repository."""
    if not snapshot and not stamp:
        path = STATE / "offsite-latest.json"
        release.protected(path, limit=16384)
        value = object_value(decode_json(path.read_bytes()))
        release.require(
            type(value.get("schemaVersion")) is int
            and value["schemaVersion"] == 1
            and value.get("repository") == configured.repository,
            "Offsite selection differs",
        )
        snapshot, stamp = string_value(value.get("snapshot")), string_value(value.get("stamp"))
    release.require(
        re.fullmatch(r"[a-f0-9]{64}", snapshot) and nightly.STAMP.fullmatch(stamp),
        "Select an exact offsite snapshot and archive timestamp",
    )
    return snapshot, stamp


def retrieve(
    runner: release.RunnerProtocol, configured: Settings, snapshot: str, target: Path, maximum: int
) -> None:
    """Stream one exact file with a kernel-enforced output-file size ceiling."""
    _ = runner.run(
        [
            "/usr/bin/prlimit",
            f"--fsize={maximum}:{maximum}",
            "--",
            *configured.command(),
            "dump",
            snapshot,
            str(nightly.TARGET / target.name),
        ],
        output_path=target,
        timeout=900,
    )


def drill(
    runner: release.RunnerProtocol, configured: Settings, snapshot: str, stamp: str
) -> JsonObject:
    """Restore the retrieved custom-format archive and attest only after exact owned cleanup."""
    snapshot, stamp = selection(configured, snapshot, stamp)
    release.require(
        shutil.disk_usage(runner.attempt).free > configured.archive_mib * MIB + 1024**3,
        "Insufficient restore download headroom",
    )
    with tempfile.TemporaryDirectory(prefix="download.", dir=runner.attempt) as temporary:
        directory = Path(temporary)
        receipt, archive = directory / f"{stamp}.receipt.json", directory / f"{stamp}.dump"
        retrieve(runner, configured, snapshot, receipt, release.MAX_BACKUP_RECEIPT_BYTES)
        retrieve(runner, configured, snapshot, archive, configured.archive_mib * MIB)
        _archive, record, _completed = release.validated_backup(receipt)
        release.require(_archive == archive, "Offsite receipt names another archive")
        image, migrations = archive_identity(record)
        report = restore.verify(runner, image, archive, migrations, limits=configured.limits)
    release.require(
        report.get("verified") is True and report.get("cleanupPassed") is True,
        "Offsite restore or cleanup failed",
    )
    report.update(
        {
            "schemaVersion": 1,
            "snapshot": snapshot,
            "backupSha256": record["sha256"],
            "finishedAt": release.timestamp(),
            "repository": configured.repository,
        }
    )
    release.atomic(runner.attempt / "outcome.json", report)
    release.atomic(
        STATE / "restore-verified.timestamp", {"evidence": str(runner.attempt), **report}
    )
    return report


class Arguments(argparse.Namespace):
    """Select an upload or isolated drill; never restore into the public database."""

    action: str = "upload"
    snapshot: str = ""
    stamp: str = ""


def remove_downloads(attempt: Path) -> None:
    """Remove only private files created by this attempt's fixed download protocol."""
    for directory in attempt.glob("download.*"):
        release.require(
            re.fullmatch(r"download\.[a-z0-9_]{8}", directory.name), "Invalid download owner"
        )
        release.protected(directory, directory=True, modes=(0o700,))
        children = list(directory.iterdir())
        release.require(len(children) <= MAX_DOWNLOAD_FILES, "Unexpected restore download contents")
        for path in children:
            release.require(
                re.fullmatch(r"[0-9]{8}T[0-9]{6}Z\.(?:dump|receipt\.json)", path.name),
                "Unrecognized restore download",
            )
            release.protected(path, limit=MAX_ARCHIVE_MIB * MIB)
        for path in children:
            path.unlink()
        directory.rmdir()
    descriptor = os.open(attempt, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def recover_interrupted(runner: release.RunnerProtocol | None = None) -> None:
    """Resolve only this workflow's recorded attempt under the already-held workload lock."""
    journal = release.WORK / "current.json"
    if not journal.exists():
        return
    release.protected(journal, limit=16384)
    current = object_value(decode_json(journal.read_bytes()))
    if current.get("finalized") is True:
        return
    release.require(
        type(current.get("schemaVersion")) is int
        and current["schemaVersion"] == 1
        and current.get("operation") == "offsite_backup"
        and current.get("action") in ("upload", "restore"),
        "Unfinished ownership belongs to another operation",
    )
    attempt = Path(string_value(current.get("attempt")))
    release.require(
        attempt.parent == STATE / "offsite"
        and re.fullmatch(str(current["action"]) + r"\.[a-z0-9_]{8}", attempt.name),
        "Invalid offsite recovery attempt",
    )
    release.protected(attempt.parent, directory=True, modes=(0o700,))
    release.protected(attempt, directory=True, modes=(0o700,))
    # A separate evidence directory preserves the interrupted command's numbered
    # files; the supplied runner exists only for deterministic local fixtures.
    if runner is None:
        recovery = Path(tempfile.mkdtemp(prefix="recovery.", dir=attempt))
        runner = release.Runner(recovery)
    restore_record = attempt / "restore.json"
    if current["action"] == "restore" and (restore_record.exists() or restore_record.is_symlink()):
        release.protected(restore_record, limit=65536)
        report = object_value(decode_json(restore_record.read_bytes()))
        restore.cleanup(
            runner,
            string_value(report.get("container")),
            string_value(report.get("postgresImage")),
            string_value(report.get("containerId")),
        )
    elif current["action"] == "restore":
        release.require(
            not runner.docker(
                "ps", "--all", "--quiet", "--filter", f"label={restore.LABEL}"
            ).strip(),
            "Unrecorded restore container requires ownership evidence",
        )
    remove_downloads(attempt)
    receipt: JsonObject = {"schemaVersion": 1, "recovered": True, "finishedAt": release.timestamp()}
    release.atomic(runner.attempt / "recovery.json", receipt)
    release.atomic(journal, {**current, "finalized": True, "passed": False, **receipt})


def main() -> None:
    """Serialize offsite work with releases and refuse missing repository credentials."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("action", choices=("upload", "restore", "recover"))
    _ = parser.add_argument("--snapshot", default="")
    _ = parser.add_argument("--stamp", default="")
    arguments = parser.parse_args(namespace=Arguments())
    release.require(os.geteuid() == 0, "Offsite backup requires root")
    _ = os.umask(0o077)

    def interrupted(_signum: int, _frame: FrameType | None) -> NoReturn:
        raise release.ReleaseError("Offsite backup interrupted")  # noqa: EM101, TRY003

    for signum in (signal.SIGTERM, signal.SIGINT):
        _ = signal.signal(signum, interrupted)
    with release.workload_lock(recover_offsite_backup=True):
        release.protected(STATE, directory=True, modes=(0o750,))
        recover_interrupted()
        if arguments.action == "recover":
            release.require(
                not arguments.snapshot and not arguments.stamp, "Recovery selects its journal"
            )
            _ = sys.stdout.write("Offsite ownership recovered; original outcome retained.\n")
            return
        configured = settings()
        attempts = STATE / "offsite"
        attempts.mkdir(mode=0o700, exist_ok=True)
        release.protected(attempts, directory=True, modes=(0o700,))
        attempt = Path(tempfile.mkdtemp(prefix=f"{arguments.action}.", dir=attempts))
        runner = release.Runner(attempt)
        current: JsonObject = {
            "schemaVersion": 1,
            "operation": "offsite_backup",
            "action": arguments.action,
            "attempt": str(attempt),
            "finalized": False,
        }
        release.atomic(release.WORK / "current.json", current)
        try:
            if arguments.action == "upload":
                release.require(
                    not arguments.snapshot and not arguments.stamp,
                    "Upload does not select a restore snapshot",
                )
                _ = upload(runner, configured)
            else:
                _ = drill(runner, configured, arguments.snapshot, arguments.stamp)
        except BaseException:
            recover_interrupted()
            raise
        release.atomic(
            release.WORK / "current.json", {**current, "finalized": True, "passed": True}
        )
    _ = sys.stdout.write("Offsite operation completed; private evidence retained.\n")


def cli() -> int:
    """Keep credential values and command error payloads out of terminal output."""
    try:
        main()
    except (OSError, ValueError, KeyError, release.ReleaseError, subprocess.SubprocessError):
        _ = sys.stderr.write("Offsite operation failed; inspect private evidence and settings.\n")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(cli())
