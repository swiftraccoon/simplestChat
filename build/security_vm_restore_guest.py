"""Run one real installed restore operation only inside its marked disposable guest.

The native binary remains unchanged. Direct client completion uses the existing
bounded pump; whole guest descendant containment is the separate QEMU owner.
"""

from __future__ import annotations

import fcntl
import hashlib
import json
import math
import os
import platform
import re
import select
import signal
import stat
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, cast, final

ROOT = Path("/root/simplestchat-vm-fixture")
LIBRARY = Path("/usr/local/libexec/simplestchat-public")
PUBLIC = Path("/srv/simplestchat-public")
STATE = Path("/var/lib/simplestchat-monitoring")
WORK = Path("/run/simplestchat-bench")
INPUT = ROOT / "restore-input"
INSTALLED = LIBRARY / "simplestchat-ops"
DOCKER = ("/usr/bin/docker", "--host", "unix:///var/run/docker.sock")
ENV = {"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LC_ALL": "C", "HOME": str(ROOT)}
MAX_RECORD = 131072
PRIVATE_MODE = 0o600
DIRECTORY_MODE = 0o700
SQL_EXECUTIONS = 3
MAX_NATIVE_READY = 40
LEDGER_LOG_OFFSET = 7
MIN_SOURCE_PINS = 7
MAX_SOURCE_PINS = 100
MAX_SEED_READY_ATTEMPTS = 60
FIXED_COMMANDS = 26
MAX_COMMANDS = 36 + MAX_SEED_READY_ATTEMPTS + FIXED_COMMANDS
PIDS = 64
NANO_CPUS = 500000000
SESSION_FAMILY_VERSION = 22
DEADLINE_STATUS = 124
MAX_STREAM = 2 * 1024**2
MAX_DUMP = 1024**2
MAX_NATIVE_COMMANDS = 73
NATIVE = 1200
OBSERVATION = 120
DEBT = NATIVE + OBSERVATION
CASES = frozenset({"current", "historical-subset-failed-release", "privacy-invalid"})
CHECKPOINTS = frozenset(
    {
        "guard",
        "input",
        "sources",
        "database",
        "seed",
        "snapshot",
        "ready",
        "native",
        "observe",
        "cleanup",
        "complete",
        "native-exit",
        "native-streams",
        "native-record",
        "native-isolation",
        "native-history",
        "privacy-refusal",
        "freshness",
        "lock",
        "source-recheck",
    }
)
FAILURES = frozenset({"refused", "io", "deadline", "cancelled", "cleanup"})
FIXTURE_LABEL = "clinic.research.simplestchat.restore-fixture"
ANCHOR = "simplestchat-public-postgres-1"
PRE_022_GRANTS = """BEGIN;
GRANT SELECT, INSERT, UPDATE ON public.users TO simplestchat_app;
GRANT SELECT, INSERT, UPDATE, DELETE
    ON public.webauthn_credentials, public.sessions, public.rooms, public.room_roles,
       public.room_states, public.room_reports, public.moderation_events, public.invites
    TO simplestchat_app;
GRANT SELECT, INSERT, DELETE ON public.invite_redemptions TO simplestchat_app;
COMMIT;"""
if __name__ == "__main__":
    sys.path[:0] = [str(ROOT), str(LIBRARY)]
import security_vm_ops_guest as ops  # noqa: E402 -- Original root-staged imports.
import security_vm_stage_guest as stage  # noqa: E402 -- Original root-staged imports.

# isort: split
import bounded_process  # noqa: E402 -- Original root-staged trusted helper.
import restore_verify as reference  # noqa: E402 -- Fixed reference resource/SQL policies only.
from release_public import LEDGER_QUERY  # noqa: E402 -- Original fixed ledger query.
from runtime_profile import validate_environment_names  # noqa: E402 -- Fixed trusted helper.

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping, Sequence
    from typing import BinaryIO


def require(value: object) -> None:
    """Use a fixed refusal without exposing SQL, config, keys or raw engine errors."""
    if not value:
        message = "restore_guest_unproven"
        raise ValueError(message)


def required[T](value: T | None) -> T:
    """Refuse absent private phase ownership before borrowing it."""
    if value is None:
        require(None)
        message = "restore_guest_missing_owner"
        raise ValueError(message)
    return value


def dictionary(value: object) -> dict[str, object]:
    """Validate object/key types before inspecting bounded private engine fields."""
    require(isinstance(value, dict))
    result = cast("dict[object, object]", value)
    require(all(isinstance(name, str) for name in result))
    return cast("dict[str, object]", result)


def exact(value: object, fields: set[str]) -> dict[str, object]:
    """Admit only a closed object before using its fields or projecting it."""
    return stage.exact(value, fields)


def digest(value: object, count: int = 64) -> str:
    """Validate an exact lowercase private identity without changing its bytes."""
    return stage.digest(value, count)


def integer(value: object, low: int, high: int) -> int:
    """Retain bool-versus-int and range distinctions."""
    return stage.integer(value, low, high)


def encoded(value: object) -> bytes:
    """Bound one closed fixed protocol frame."""
    data = json.dumps(value, separators=(",", ":"), allow_nan=False).encode() + b"\n"
    require(len(data) <= MAX_RECORD)
    return data


@final
class Control:
    """Keep entry-time setup/body/end and first signal without a refreshed clock."""

    def __init__(self, started: float) -> None:
        """Permit only bounded initial input before decoding remaining durations."""
        self.started = started
        self.setup_end = started + 30
        self.body = started + 1680
        self.end = self.body + 30
        self.setup = True
        self.settling = False
        self.status = 0
        self.checkpoint = "input"

    @property
    def body_end(self) -> float:
        """Keep seed work inside setup until the final live GO admission."""
        return min(self.setup_end, self.body) if self.setup else self.body

    def signal(self, number: int, _frame: object) -> None:
        """Latch cancellation until all known owners settle."""
        self.status = self.status or (130 if number == signal.SIGINT else 143)

    def check(self) -> None:
        """Fence original time and cancellation without pretending an I/O hard wall."""
        now = time.monotonic()
        require(math.isfinite(now) and self.started <= now)
        if now >= (self.end if self.settling else self.body_end):
            self.status = self.status or 124
            require(None)
        require(not self.status or self.settling)

    def configure(self, value: Mapping[str, object]) -> None:
        """Narrow from controller remaining durations while keeping the same entry."""
        self.check()
        self.setup_end = self.started + integer(value["setupSeconds"], 1, 360)
        self.body = self.started + integer(value["bodySeconds"], DEBT, 1680)
        self.end = self.body + 30
        self.check()

    def advance(self) -> None:
        """Check setup and full remaining debt BEFORE widening this phase."""
        self.check()
        require(self.setup and self.body - time.monotonic() >= DEBT)
        self.check()
        self.setup = False


@final
class Wire:
    """Own one duplicated stdin while restoring its original shared-OFD flags."""

    def __init__(self, control: Control) -> None:
        """Register the duplicate before any fallible flag mutation."""
        self.control, self.buffer, self.fd = control, bytearray(), -1
        self.flags: int | None = None
        control.check()
        self.fd = os.dup(sys.stdin.fileno())
        try:
            self.flags = fcntl.fcntl(self.fd, fcntl.F_GETFL)
            _ = fcntl.fcntl(self.fd, fcntl.F_SETFL, self.flags | os.O_NONBLOCK)
            control.check()
        except BaseException as error:
            first = ops.settle((self.close,), error)
            if first is not None:
                raise first from None
            raise

    def line(self) -> bytes:
        """Read one physical bounded LF frame with original post-read fences."""
        while True:
            self.control.check()
            if b"\n" in self.buffer:
                line, _, rest = self.buffer.partition(b"\n")
                self.buffer = bytearray(rest)
                return bytes(line)
            require(len(self.buffer) < MAX_RECORD)
            try:
                chunk = os.read(self.fd, MAX_RECORD + 1 - len(self.buffer))
            except (BlockingIOError, InterruptedError):
                _ = select.select([self.fd], [], [], 0.05)
                continue
            self.control.check()
            require(chunk)
            self.buffer.extend(chunk)
            require(len(self.buffer) <= MAX_RECORD)

    def no_extra(self) -> None:
        """Reject already delivered extra commands; future data supplies no authority."""
        self.control.check()
        require(not self.buffer)
        try:
            _ = os.read(self.fd, 1)
        except (BlockingIOError, InterruptedError):
            self.control.check()
            return
        self.control.check()
        # Either unexpected bytes or peer EOF invalidates a positive admission.
        require(None)

    def emit(self, value: object) -> None:
        """Keep both actual writes and flush inside the original phase."""
        self.control.check()
        data = encoded(value)
        require(sys.stdout.buffer.write(data) == len(data))
        _ = sys.stdout.buffer.flush()
        self.control.check()

    def close(self) -> None:
        """Attempt original flag restoration and duplicate close independently."""
        if self.fd < 0:
            return
        descriptor, self.fd = self.fd, -1
        actions: list[Callable[[], None]] = []
        flags = self.flags
        if flags is not None:

            def restore() -> None:
                _ = fcntl.fcntl(descriptor, fcntl.F_SETFL, flags)

            actions.append(restore)
        actions.append(lambda: os.close(descriptor))
        first = ops.settle(actions)
        if first is not None:
            raise first


def source_path(name: str) -> tuple[Path, int]:
    """Map only the fixed source closure; never accept an arbitrary guest path."""
    if name in {"build/security_vm_restore_guest.py", "build/security_vm_stage_guest.py"}:
        return ROOT / Path(name).name, 0o600
    if name in {"ops/ansible/files/restore_verify.py", "ops/ansible/files/restore-verify.sql"}:
        return LIBRARY / Path(name).name, 0o644
    if name in {
        "ops/ansible/files/monitoring-schema.sql",
        "ops/ansible/templates/public-runtime-grants.sql.j2",
    }:
        return INPUT / Path(name).name, 0o600
    require(re.fullmatch(r"migrations/[0-9]{3}_[a-z0-9_]+\.sql", name))
    return INPUT / name, 0o600


def request(data: bytes) -> dict[str, object]:
    """Decode one exact controller request before any database effects."""
    value = exact(
        ops.object_json(data),
        {
            "schemaVersion",
            "scope",
            "runId",
            "revision",
            "scenario",
            "setupSeconds",
            "bodySeconds",
            "sourcePins",
            "binary",
            "postgresImage",
            "engineVersion",
        },
    )
    require(type(value["schemaVersion"]) is int and value["schemaVersion"] == 1)
    require(value["scope"] == "RestoreVerificationRequest")
    _ = digest(value["runId"], 32)
    _ = digest(value["revision"], 40)
    require(isinstance(value["scenario"], str) and value["scenario"] in CASES)
    binary = exact(value["binary"], {"sha256", "bytes"})
    _ = digest(binary["sha256"])
    _ = integer(binary["bytes"], 1, ops.MAX_BINARY)
    require(
        isinstance(value["postgresImage"], str)
        and re.fullmatch(
            r"docker.io/library/postgres:18\.6-bookworm@sha256:[a-f0-9]{64}", value["postgresImage"]
        )
    )
    require(
        isinstance(value["engineVersion"], str)
        and re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", value["engineVersion"])
    )
    pins = value["sourcePins"]
    require(
        isinstance(pins, dict)
        and MIN_SOURCE_PINS <= len(cast("dict[object, object]", pins)) <= MAX_SOURCE_PINS
    )
    names: set[str] = set()
    for name, checksum in cast("dict[object, object]", pins).items():
        require(isinstance(name, str))
        name = cast("str", name)
        _ = source_path(name)
        _ = digest(checksum)
        names.add(name)
    require(
        {
            "build/security_vm_restore_guest.py",
            "build/security_vm_stage_guest.py",
            "ops/ansible/files/restore_verify.py",
            "ops/ansible/files/restore-verify.sql",
            "ops/ansible/files/monitoring-schema.sql",
            "ops/ansible/templates/public-runtime-grants.sql.j2",
        }
        <= names
    )
    return value


def inspection(data: bytes) -> list[object]:
    """Use the existing duplicate/nonfinite-refusing byte decoder for engine arrays."""
    require(len(data) <= MAX_STREAM)
    value = ops.object_json(b'{"inspection":' + data + b"}")["inspection"]
    require(isinstance(value, list))
    return cast("list[object]", value)


def native_projection(value: object, scenario: str) -> dict[str, object]:
    """Project fixed fixture semantics, never raw result/config/stream text."""
    fields = {
        "containerId",
        "postgresImage",
        "sourceUnchanged",
        "sourceLedgerAndCountsUnchanged",
        "snapshotAbsent",
        "labelAbsent",
        "isolationInspected",
        "actorRemoved",
        "freshnessMatched",
        "freshnessPreserved",
        "lockReacquired",
        "verified",
        "cleanupPassed",
        "counts",
        "backupSha256",
        "outcomePresent",
        "privacyRefusalObserved",
        "nativeRawExit",
        "engineVersion",
        "postgresVersionMatched",
        "historyObserved",
        "restoreExecObserved",
        "amcheckExecObserved",
    }
    row = exact(value, fields)
    _ = digest(row["containerId"])
    require(
        isinstance(row["postgresImage"], str)
        and re.fullmatch(r"sha256:[a-f0-9]{64}", row["postgresImage"])
    )
    _ = digest(row["backupSha256"])
    require(
        isinstance(row["engineVersion"], str)
        and re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", row["engineVersion"])
    )
    for field in fields - {
        "containerId",
        "postgresImage",
        "backupSha256",
        "counts",
        "nativeRawExit",
        "engineVersion",
    }:
        require(type(row[field]) is bool)
    negative = scenario == "privacy-invalid"
    require(type(row["nativeRawExit"]) is int and row["nativeRawExit"] == int(negative))
    for field in (
        "sourceUnchanged",
        "sourceLedgerAndCountsUnchanged",
        "snapshotAbsent",
        "labelAbsent",
        "isolationInspected",
        "actorRemoved",
        "lockReacquired",
        "cleanupPassed",
        "postgresVersionMatched",
        "historyObserved",
        "restoreExecObserved",
    ):
        require(row[field] is True)
    require(row["verified"] is not negative and row["outcomePresent"] is not negative)
    require(row["historyObserved"] is True and row["restoreExecObserved"] is True)
    require(row["amcheckExecObserved"] is not negative)
    require(row["privacyRefusalObserved"] is negative)
    require(row["freshnessPreserved"] is negative and row["freshnessMatched"] is not negative)
    if negative:
        require(row["counts"] is None)
    else:
        expected = {
            "users": 1,
            "rooms": 1,
            "sessions": int(scenario.startswith("historical")),
            "credentials": 0,
            "activeIncidents": 1,
            "resolvedIncidents": 1,
        }
        counts = exact(row["counts"], set(expected))
        require(counts == expected and all(type(v) is int for v in counts.values()))
    return row


@dataclass(frozen=True)
class Result:
    """Keep actual signed status separate from bounded private stream bytes."""

    status: int
    output: bytes
    error: bytes


@final
class Commands:
    """Use the existing direct-child pump without post-reap signalling claims."""

    def __init__(self, control: Control, originals: list[stage.Original]) -> None:
        """Keep every current child/log original in this guest transaction."""
        self.control, self.originals = control, originals
        self.count = 0
        self.native: stage.Original | None = None
        self.observer: Callable[[], None] | None = None
        self.observing = False
        self.observer_error: BaseException | None = None
        self.last_status: int | None = None
        self.last_error: Path | None = None
        self.native_status: int | None = None
        self.peer: Wire | None = None
        self.creating_seed = False
        self.created_seed: str | None = None

    def healthy(self) -> bool:
        """Check live owner and run only the read-only nonrecursive native observer."""
        try:
            self.control.check()
            if self.peer is not None and not self.control.settling:
                self.peer.no_extra()
            if self.observer is not None and not self.observing:
                self.observing = True
                try:
                    self.observer()
                finally:
                    self.observing = False
        except (OSError, ValueError, RuntimeError) as error:
            if self.observer is not None:
                self.observer_error = self.observer_error or error
            return False
        return True

    def run(  # noqa: C901, PLR0912, PLR0915 -- One current child/pipe/log owner.
        self,
        arguments: Sequence[str],
        *,
        data: bytes = b"",
        timeout: float = 30,
        native: bool = False,
    ) -> Result:
        """Retain both sinks before child admission and preserve first through all closes."""
        self.control.check()
        if self.peer is not None and not self.control.settling:
            self.peer.no_extra()
        for original in self.originals:
            require(ops.stamp(os.fstat(original.file.fileno())) == ops.stamp(original.before))
            require(ops.stamp(original.path.lstat()) == ops.stamp(original.before))
        candidate = self.native if native else None
        if native:
            require(candidate is not None)
        self.count += 1
        require(self.count <= MAX_COMMANDS)
        streams: list[BinaryIO] = []
        child: subprocess.Popen[bytes] | None = None
        first: BaseException | None = None
        consumed = False
        status, output, error = 1, b"", b""
        try:
            for suffix in ("stdout", "stderr"):
                self.control.check()
                path = ROOT / f"restore-observer-{self.count:03}.{suffix}"
                descriptor = os.open(
                    path, os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600
                )
                streams.append(stage.adopt(descriptor, "w+b"))
                self.control.check()
                if suffix == "stderr":
                    self.last_error = path
            if candidate is not None:
                candidate.check()
            ceiling = self.control.body if self.control.settling else self.control.body_end
            if native:
                require(ceiling - time.monotonic() >= DEBT)
            command_end = min(time.monotonic() + timeout, ceiling)
            require(command_end > time.monotonic())
            child = subprocess.Popen(  # noqa: S603 -- Fixed argv/original.
                ["simplestchat-ops", *arguments]
                if candidate is not None
                else [*DOCKER, *arguments],
                executable="/proc/self/fd/" + str(candidate.file.fileno())
                if candidate is not None
                else DOCKER[0],
                pass_fds=(candidate.file.fileno(),) if candidate is not None else (),
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
                cwd=ROOT,
                env=ENV,
            )
            # The child is registered before the original post-spawn fence.
            self.control.check()
            remaining = command_end - time.monotonic()
            require(remaining > 0)
            status = bounded_process.pump(
                child,
                streams[0],
                streams[1],
                bounded_process.Limits(
                    timeout=remaining, stdout=MAX_STREAM, stderr=MAX_STREAM, healthy=self.healthy
                ),
                input_data=data,
            )
            consumed = True
            self.last_status = status
            if native:
                self.native_status = status
            for stream in streams:
                stream.flush()
            _ = streams[0].seek(0)
            _ = streams[1].seek(0)
            output, error = streams[0].read(MAX_STREAM + 1), streams[1].read(MAX_STREAM + 1)
            require(len(output) <= MAX_STREAM and len(error) <= MAX_STREAM)
            if self.creating_seed and status == 0 and re.fullmatch(rb"[a-f0-9]{64}\s*", output):
                self.created_seed = output.decode().strip()
            if candidate is not None:
                candidate.check()
        except BaseException as caught:  # noqa: BLE001 -- Close all known peers.
            first = (self.observer_error or caught) if native else caught
            if child is not None and not consumed and child.returncode is None:
                first = ops.settle((lambda: bounded_process.stop(child),), first)
        finally:
            actions: list[Callable[[], None]] = []
            if child is not None:
                actions.extend(
                    stream.close
                    for stream in (child.stdin, child.stdout, child.stderr)
                    if stream is not None
                )
            actions.extend(stream.close for stream in streams)
            first = ops.settle(actions, first)
        if first is not None:
            raise first
        self.control.check()
        return Result(status, output, error)

    def checked(self, arguments: Sequence[str], *, data: bytes = b"", timeout: float = 30) -> bytes:
        """Only actual completed0 supplies a normal fixed setup/observation result."""
        result = self.run(arguments, data=data, timeout=timeout)
        require(result.status == 0)
        return result.output


def write_private(path: Path, data: bytes, control: Control) -> None:
    """Create0600 and fence each bounded physical write/flush/fsync before checked close."""
    control.check()
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    stream = stage.adopt(descriptor, "wb")
    first: BaseException | None = None
    try:
        control.check()
        for offset in range(0, len(data), 65536):
            control.check()
            chunk = data[offset : offset + 65536]
            require(stream.write(chunk) == len(chunk))
            control.check()
        control.check()
        stream.flush()
        control.check()
        os.fsync(stream.fileno())
        control.check()
    except BaseException as error:  # noqa: BLE001 -- Original close must run after body refusal.
        first = error
    finally:
        first = ops.settle((stream.close,), first)
    if first is not None:
        raise first


def absent(path: Path, control: Control) -> bool:
    """Accept only an actual no-follow missing entry, never a dangling symlink."""
    control.check()
    try:
        _ = path.lstat()
    except FileNotFoundError:
        control.check()
        return True
    control.check()
    return False


def mutable_hint(path: Path, control: Control) -> dict[str, object] | None:
    """Read a passive atomic-generation hint; only later engine/final joins prove facts."""
    control.check()
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    except FileNotFoundError:
        control.check()
        return None
    stream = stage.adopt(descriptor, "rb")
    first: BaseException | None = None
    result: dict[str, object] | None = None
    try:
        control.check()
        before = os.fstat(stream.fileno())
        require(stat.S_ISREG(before.st_mode) and before.st_uid == before.st_gid == 0)
        require(stat.S_IMODE(before.st_mode) == PRIVATE_MODE and before.st_nlink in (0, 1))
        require(0 < before.st_size <= MAX_RECORD)
        data = stream.read(MAX_RECORD + 1)
        control.check()
        after = os.fstat(stream.fileno())
        # Native atomically replaces initial/created/final generations. If this
        # sample changed, discard only this non-authoritative hint and retry the
        # existing bounded healthy observation. No Source/Record witness is relaxed.
        if ops.stamp(before) == ops.stamp(after):
            require(len(data) == before.st_size)
            result = ops.object_json(data)
    except BaseException as error:  # noqa: BLE001 -- Preserve read before close.
        first = error
    finally:
        first = ops.settle((stream.close,), first)
    if first is not None:
        raise first
    return result


def classification(error: BaseException, status: int) -> str:
    """Project the first observed cause without exposing external error text."""
    if status:
        return "deadline" if status == DEADLINE_STATUS else "cancelled"
    return "io" if isinstance(error, OSError) else "refused"


def json_record(path: Path, control: Control, originals: list[stage.Original]) -> dict[str, object]:
    """Read one retained private original rather than reopening its JSON path."""
    original = stage.Original(
        path, MAX_RECORD, 0o600, cast("stage.Control", cast("object", control))
    )
    originals.append(original)
    return ops.object_json(original.data())


TMPFS_TARGETS = frozenset(
    {
        "/var/lib/postgresql",
        "/var/run/postgresql",
        "/tmp",  # noqa: S108 -- Container tmpfs only.
    }
)


def isolation(value: list[object], expected: Mapping[str, str]) -> dict[str, object]:
    """Inspect only the actual generated joined native actor and its fixed restrictions."""
    require(len(value) == 1)
    item = dictionary(value[0])
    config = dictionary(item.get("Config"))
    host = dictionary(item.get("HostConfig"))
    labels = dictionary(config.get("Labels"))
    require(labels.get(reference.LABEL) == expected["name"])
    identifier = digest(item.get("Id"))
    require(item.get("Name") == "/" + expected["name"] and item.get("Image") == expected["image"])
    require(config.get("User") == "999:999" and host.get("NetworkMode") == "none")
    require(host.get("ReadonlyRootfs") is True and host.get("Privileged") is False)
    require(host.get("CapDrop") == ["ALL"] and host.get("CapAdd") in (None, []))
    require(host.get("SecurityOpt") in (["no-new-privileges:true"], ["no-new-privileges"]))
    require(host.get("Memory") == host.get("MemorySwap") == 512 * 1024**2)
    require(host.get("PidsLimit") == PIDS and host.get("NanoCpus") == NANO_CPUS)
    require(host.get("ShmSize") == 16 * 1024**2)
    require(dictionary(host.get("LogConfig")).get("Type") == "none")
    require(config.get("Entrypoint") == ["docker-entrypoint.sh"])
    arguments = reference.create_arguments(expected["name"], expected["image"])
    require(config.get("Cmd") == arguments[arguments.index(expected["image"]) + 1 :])
    variables = config.get("Env")
    require(isinstance(variables, list))
    values = cast("list[object]", variables)
    require(all(isinstance(value, str) for value in values))
    strings = cast("list[str]", values)
    names = [value.split("=", 1)[0] for value in strings]
    require(len(names) == len(set(names)))
    validate_environment_names(names)
    require(
        {
            "PGDATA=/var/lib/postgresql/data",
            "POSTGRES_DB=restorecheck",
            "POSTGRES_HOST_AUTH_METHOD=trust",
            "POSTGRES_INITDB_ARGS=--auth-local=peer --auth-host=reject",
        }
        <= set(strings)
    )
    mounts = item.get("Mounts")
    require(isinstance(mounts, list))
    targets: set[str] = set()
    for raw in cast("list[object]", mounts):
        mount = dictionary(raw)
        require(mount.get("Type") == "tmpfs" and mount.get("RW") is True)
        target = mount.get("Destination")
        require(isinstance(target, str) and target in TMPFS_TARGETS)
        require(target not in targets and mount.get("Source") in (None, ""))
        targets.add(cast("str", target))
    tmpfs = host.get("Tmpfs")
    require(
        tmpfs
        == {
            "/var/lib/postgresql": "rw,nosuid,nodev,noexec,size=256m,uid=999,gid=999,mode=0700",
            "/var/run/postgresql": "rw,nosuid,nodev,noexec,size=1m,uid=999,gid=999,mode=0700",
            "/tmp": "rw,nosuid,nodev,noexec,size=16m,mode=1777",  # noqa: S108 -- Private tmpfs.
        }
    )
    return {"id": identifier, "name": expected["name"], "image": expected["image"]}


def execution_kind(command: str) -> str:
    """Classify only the four fixed native private-socket invocation spellings."""
    connection = " -h " + reference.SOCKET + " -U postgres -d " + reference.DATABASE
    choices = {
        "psql -X -q -t -A --set ON_ERROR_STOP=1" + connection: "sql",
        "timeout --signal=TERM --kill-after=2s 120s pg_restore "
        + "--exit-on-error --single-transaction"
        + connection: "restore",
        "timeout --signal=TERM --kill-after=2s 120s pg_amcheck "
        + "--install-missing --heapallindexed --parent-check"
        + connection: "amcheck",
        '/bin/sh -c test "$(cat /proc/1/comm)" = postgres && '
        + "pg_isready -q"
        + connection: "ready",
    }
    require(command in choices)
    return choices[command]


def execution_sequence(executions: list[str], statuses: list[int], *, negative: bool) -> int:
    """Require completed role/status facts in original create order, not event-delivery order."""
    require(len(executions) == len(statuses))
    ready = executions.count("ready")
    require(
        executions.count("restore") == 1
        and executions.count("sql") == SQL_EXECUTIONS
        and executions.count("amcheck") == int(not negative)
    )
    require(1 <= ready <= MAX_NATIVE_READY)
    require(
        executions
        == ["ready"] * ready + ["sql", "restore", "sql", "sql"] + ([] if negative else ["amcheck"])
    )
    require(all(status != 0 for status in statuses[: ready - 1]))
    require(statuses[ready - 1 : ready + 3] == [0, 0, 0, 0])
    if negative:
        require(statuses[-1] != 0)
    else:
        require(statuses[ready + 3 :] == [0, 0])
    return ready


@final
class Fixture:
    """Own source database/input originals and observe one real native actor."""

    def __init__(self, control: Control, value: Mapping[str, object], wire: Wire) -> None:
        """Register owners before any source or engine effects."""
        self.control, self.value, self.wire = control, value, wire
        self.originals: list[stage.Original] = []
        self.commands = Commands(control, self.originals)
        self.commands.peer = wire
        self.sources: dict[str, stage.Original] = {}
        self.seed_id: str | None = None
        self.seed_image = ""
        self.native_actor: dict[str, object] | None = None
        self.observation_started = ""
        self.observation_attempted = False
        self.attempts_before: set[str] = set()
        self.archive: stage.Original | None = None
        self.prior_freshness: stage.Original | None = None
        self.lock_before: os.stat_result | None = None
        self.namespaces: list[tuple[Path, int, os.stat_result, set[str]]] = []
        self.source_ledger = b""
        self.source_counts: dict[str, object] = {}

    def retain(self, path: Path, mode: int, maximum: int = MAX_RECORD) -> stage.Original:
        """Keep every admitted original even if a later comparison refuses."""
        original = stage.Original(
            path, maximum, mode, cast("stage.Control", cast("object", self.control))
        )
        self.originals.append(original)
        return original

    def namespace(self, path: Path, names: set[str]) -> None:
        """Register a readonly input-directory original before its fallible admission."""
        self.control.check()
        descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
        before: os.stat_result | None = None
        try:
            before = os.fstat(descriptor)
            self.namespaces.append((path, descriptor, before, names))
            self.control.check()
            require(
                before.st_uid == before.st_gid == 0 and (before.st_mode & 0o7777) == DIRECTORY_MODE
            )
            self.namespace_check(path, descriptor, before, names)
        except BaseException as error:
            # If fstat failed, this descriptor was not yet registered with the fixture.
            if before is None:
                first = ops.settle((lambda: os.close(descriptor),), error)
                if first is not None:
                    raise first from None
            raise

    def namespace_check(
        self, path: Path, descriptor: int, before: os.stat_result, names: set[str]
    ) -> None:
        """Bind original directory and exact declared members without trusting a saved map."""
        self.control.check()
        require(ops.stamp(os.fstat(descriptor)) == ops.stamp(before))
        require(ops.stamp(path.lstat()) == ops.stamp(before))
        require(set(os.listdir(descriptor)) == names)
        self.control.check()

    def sql(self, source: str) -> bytes:
        """Send source-fixture SQL only to the retained source CID private socket."""
        require(self.seed_id is not None)
        return self.commands.checked(
            [
                "exec",
                "-i",
                "--user",
                "999:999",
                reference_id(required(self.seed_id)),
                "psql",
                "-X",
                "-q",
                "-t",
                "-A",
                "--set",
                "ON_ERROR_STOP=1",
                "-h",
                reference.SOCKET,
                "-U",
                "postgres",
                "-d",
                reference.DATABASE,
            ],
            data=source.encode(),
            timeout=45,
        )

    def counts(self) -> dict[str, object]:
        """Read only synthetic fixed aggregates from the original source database."""
        return ops.object_json(
            self.sql("""BEGIN READ ONLY;
SELECT json_build_object('users',(SELECT count(*) FROM public.users),
'rooms',(SELECT count(*) FROM public.rooms),
'sessions',(SELECT count(*) FROM public.sessions),
'credentials',(SELECT count(*) FROM public.webauthn_credentials),
'activeIncidents',(SELECT count(*) FROM operations.alerts WHERE resolved_at IS NULL),
'resolvedIncidents',(SELECT count(*) FROM operations.alerts WHERE resolved_at IS NOT NULL));
COMMIT;""")
        )

    def setup(self) -> None:  # noqa: PLR0915 -- Fixed seed effect order.
        """Prepare real pinned PostgreSQL/SQL/dump inputs, never an app deployment."""
        self.control.checkpoint = "sources"
        require(os.geteuid() == 0 and sys.platform == "linux" and platform.machine() == "x86_64")
        marker = self.retain(Path("/etc/simplestchat-vm-fixture"), 0o600)
        require(marker.data().decode() == cast("str", self.value["runId"]) + "\n")
        pins = cast("dict[str, object]", self.value["sourcePins"])
        self.namespace(
            INPUT,
            {
                "monitoring-schema.sql",
                "public-runtime-grants.sql.j2",
                "postgres-image.txt",
                "migrations",
            },
        )
        self.namespace(
            INPUT / "migrations",
            {Path(name).name for name in pins if name.startswith("migrations/")},
        )
        for name, checksum in pins.items():
            path, mode = source_path(name)
            original = self.retain(path, mode, 2**20)
            require(original.sha256 == checksum)
            self.sources[name] = original
        candidate = self.retain(INSTALLED, 0o755, ops.MAX_BINARY)
        binary = cast("dict[str, object]", self.value["binary"])
        require(
            candidate.sha256 == binary["sha256"] and candidate.before.st_size == binary["bytes"]
        )
        self.commands.native = candidate
        self.control.checkpoint = "database"
        configured = self.retain(INPUT / "postgres-image.txt", 0o600).data().decode().strip()
        require(configured == self.value["postgresImage"])
        version = (
            self.commands.checked(["version", "--format", "{{.Server.Version}}"]).decode().strip()
        )
        require(version == self.value["engineVersion"])
        _ = self.retain(Path("/usr/bin/docker"), 0o755, ops.MAX_BINARY)
        _ = self.commands.checked(["pull", configured], timeout=180)
        inspected = inspection(self.commands.checked(["image", "inspect", configured]))
        require(len(inspected) == 1)
        image = dictionary(inspected[0])
        raw_image = image.get("Id")
        require(isinstance(raw_image, str) and re.fullmatch(r"sha256:[a-f0-9]{64}", raw_image))
        self.seed_image = cast("str", raw_image)
        require(image.get("Os") == "linux" and image.get("Architecture") == "amd64")
        repo_digests = image.get("RepoDigests")
        require(isinstance(repo_digests, list))
        observed = cast("list[object]", repo_digests)
        require(all(isinstance(item, str) for item in observed))
        expected_digest = configured.split("@", 1)[1]
        require(
            any(
                item
                in {
                    name + "@" + expected_digest
                    for name in ("postgres", "library/postgres", "docker.io/library/postgres")
                }
                for item in observed
            )
        )
        require(
            not self.commands.checked(
                ["ps", "--all", "--quiet", "--no-trunc", "--filter", "name=^/" + ANCHOR + "$"]
            ).strip()
        )
        arguments = reference.create_arguments(ANCHOR, self.seed_image)
        arguments[arguments.index("--label") + 1] = (
            FIXTURE_LABEL + "=" + cast("str", self.value["runId"])
        )
        self.commands.creating_seed = True
        try:
            raw = self.commands.checked(arguments).decode().strip()
            self.seed_id = digest(raw)
        finally:
            self.commands.creating_seed = False
            self.seed_id = self.seed_id or self.commands.created_seed
        _ = self.commands.checked(["start", required(self.seed_id)])
        ready_end = min(time.monotonic() + 30, self.control.body_end)
        attempts = 0
        while True:
            remaining = ready_end - time.monotonic()
            require(remaining > 0)
            attempts += 1
            require(attempts <= MAX_SEED_READY_ATTEMPTS)
            result = self.commands.run(
                [
                    "exec",
                    "--user",
                    "999:999",
                    required(self.seed_id),
                    "pg_isready",
                    "-q",
                    "-h",
                    reference.SOCKET,
                    "-U",
                    "postgres",
                    "-d",
                    reference.DATABASE,
                ],
                timeout=min(3, remaining),
            )
            require(time.monotonic() < ready_end)
            if result.status == 0:
                break
            require(time.monotonic() + 0.5 < ready_end)
            time.sleep(0.5)
            self.control.check()
        version = self.commands.checked(
            ["exec", "--user", "999:999", required(self.seed_id), "postgres", "--version"]
        )
        require(version.startswith(b"postgres (PostgreSQL) 18.6"))
        self.control.checkpoint = "seed"
        _ = self.sql(
            reference.ROLE_SQL.decode()
            + """
REVOKE ALL ON SCHEMA public FROM PUBLIC;
GRANT USAGE,CREATE ON SCHEMA public TO simplestchat_migrate;
GRANT USAGE ON SCHEMA public TO simplestchat_app;
CREATE EXTENSION pg_trgm;
SET ROLE simplestchat_migrate;
CREATE TABLE public._sqlx_migrations(version BIGINT PRIMARY KEY,
 success BOOLEAN NOT NULL, checksum BYTEA NOT NULL);
"""
        )
        packaged: dict[str, str] = {}
        selected: dict[str, str] = {}
        historical = self.value["scenario"] == "historical-subset-failed-release"
        for name in sorted(self.sources):
            if not name.startswith("migrations/"):
                continue
            original = self.sources[name]
            data = original.data(2**20)
            version = int(Path(name).name.partition("_")[0])
            checksum = hashlib.sha384(data).hexdigest()
            packaged[str(version)] = checksum
            if historical and version >= SESSION_FAMILY_VERSION:
                continue
            text = data.decode()
            transactional = not text.startswith("-- no-transaction\n")
            prefix = (
                "BEGIN;\nSET LOCAL ROLE simplestchat_migrate;\n"
                if transactional
                else "SET ROLE simplestchat_migrate;\n"
            )
            _ = self.sql(
                prefix
                + text
                + f"\nINSERT INTO public._sqlx_migrations VALUES ({version},true,"
                + f"decode('{checksum}','hex'));\n"
                + ("COMMIT;" if transactional else "")
            )
            selected[str(version)] = checksum
        require(set(packaged) == {str(i) for i in range(1, 37)})
        _ = self.sql(
            PRE_022_GRANTS
            if historical
            else self.sources["ops/ansible/templates/public-runtime-grants.sql.j2"].data().decode()
        )
        _ = self.sql(
            "SET ROLE postgres;\n"
            + self.sources["ops/ansible/files/monitoring-schema.sql"].data().decode()
        )
        _ = self.sql("""INSERT INTO public.users(id,email,display_name)
VALUES ('00000000-0000-0000-0000-000000000001','fixture@example.test','Fixture');
INSERT INTO public.rooms(id,owner_id,display_name)
VALUES ('fixture','00000000-0000-0000-0000-000000000001','Fixture');
INSERT INTO operations.alerts(incident_key,rule,severity,resource,first_seen,last_seen,resolved_at)
VALUES (repeat('a',64),'DatabaseUnavailable','critical','{}',now(),now(),NULL),
       (repeat('b',64),'CollectorStale','warning','{}',now(),now(),now());
INSERT INTO operations.alert_cursor VALUES (true,now());""")
        if historical:
            _ = self.sql(
                "INSERT INTO public.sessions(user_id,refresh_token_hash,expires_at) "
                + "VALUES ('00000000-0000-0000-0000-000000000001',"
                + "repeat('c',64),now()+interval '1 day');"
            )
        if self.value["scenario"] == "privacy-invalid":
            _ = self.sql("GRANT USAGE ON SCHEMA operations TO simplestchat_app;")
        self.source_ledger = self.sql(LEDGER_QUERY).strip()
        expected_ledger = "\n".join(
            f"{key}|t|{value}"
            for key, value in sorted(selected.items(), key=lambda item: int(item[0]))
        )
        require(self.source_ledger == expected_ledger.encode())
        self.source_counts = self.counts()
        require(
            self.source_counts
            == {
                "users": 1,
                "rooms": 1,
                "sessions": int(historical),
                "credentials": 0,
                "activeIncidents": 1,
                "resolvedIncidents": 1,
            }
        )
        self.control.checkpoint = "snapshot"
        dump = self.commands.checked(
            [
                "exec",
                "--user",
                "999:999",
                required(self.seed_id),
                "pg_dump",
                "--format=custom",
                "-h",
                reference.SOCKET,
                "-U",
                "postgres",
                "-d",
                reference.DATABASE,
            ],
            timeout=60,
        )
        require(0 < len(dump) <= MAX_DUMP and dump.startswith(b"PGDMP"))
        selected_attempt = PUBLIC / "results" / ("release." + cast("str", self.value["runId"]))
        selected_attempt.mkdir(mode=0o700)
        release = PUBLIC / "releases" / cast("str", self.value["revision"])
        release.mkdir(mode=0o700)
        write_private(selected_attempt / "database-before.dump", dump, self.control)
        self.archive = self.retain(selected_attempt / "database-before.dump", 0o600, MAX_DUMP)
        outcome = {
            "action": "maintain",
            "passed": not historical,
            "revision": self.value["revision"],
            "backupSha256": required(self.archive).sha256,
            "backupMigrations": selected,
        }
        write_private(selected_attempt / "outcome.json", encoded(outcome), self.control)
        _ = self.retain(selected_attempt / "outcome.json", 0o600)
        manifest = {
            "schemaVersion": 1,
            "revision": self.value["revision"],
            "platform": "linux/amd64",
            "archiveSha256": hashlib.sha256(b"fixture-inactive-image-archive").hexdigest(),
            "imageTag": "simplestchat-release/production:" + cast("str", self.value["revision"]),
            "migrations": packaged,
            "createdAt": "2026-10-25T00:00:00Z",
        }
        write_private(release / "release.json", encoded(manifest), self.control)
        _ = self.retain(release / "release.json", 0o600)
        lock = WORK / "workload.lock"
        if not lock.exists():
            write_private(lock, b"", self.control)
        self.lock_before = self.retain(lock, 0o600).before
        write_private(
            STATE / "restore-verified.timestamp", encoded({"fixturePrevious": True}), self.control
        )
        self.prior_freshness = stage.Original(
            STATE / "restore-verified.timestamp",
            MAX_RECORD,
            0o600,
            cast("stage.Control", cast("object", self.control)),
        )
        self.attempts_before = {p.name for p in (STATE / "restores").iterdir()}
        self.control.checkpoint = "ready"

    def observe_actor(self) -> None:
        """Use a passive created-record hint for ONE actual joined inspect sample."""
        if self.observation_attempted:
            return
        self.control.check()
        attempts = [p for p in (STATE / "restores").iterdir() if p.name not in self.attempts_before]
        require(len(attempts) <= 1)
        if not attempts:
            return
        require(re.fullmatch(r"restore\.[a-f0-9]{32}", attempts[0].name))
        hint = mutable_hint(attempts[0] / "restore.json", self.control)
        if hint is None:
            return
        identifier = hint.get("containerId")
        if identifier == "":
            return
        identifier = digest(identifier)
        name = hint.get("container")
        require(isinstance(name, str) and re.fullmatch(r"scpub-restore-[a-f0-9]{32}", name))
        # The hint authorizes no mutation. Only real engine identity/profile is observed.
        self.observation_attempted = True
        raw = self.commands.run(["inspect", identifier], timeout=2)
        if raw.status != 0:
            return
        self.native_actor = isolation(
            inspection(raw.output), {"name": cast("str", name), "image": self.seed_image}
        )

    def history(self, identifier: str, *, negative: bool) -> int:
        """Join bounded actual daemon history without confusing PG removal with natural0."""
        data = self.commands.checked(
            [
                "events",
                "--since",
                self.observation_started,
                "--until",
                stage.utc(),
                "--filter",
                "container=" + identifier,
                "--format",
                "{{json .}}",
            ]
        )
        rows = data.splitlines()
        require(0 < len(rows) <= stage.EVENT_HISTORY)
        lifecycle: list[str] = []
        executions: dict[str, str] = {}
        started: set[str] = set()
        statuses: dict[str, int] = {}
        for line in rows:
            value = ops.object_json(line)
            require(value.get("Type") == "container")
            actor = dictionary(value.get("Actor"))
            require(actor.get("ID") == identifier)
            attributes = dictionary(actor.get("Attributes"))
            require(attributes.get("name") == required(self.native_actor)["name"])
            require(attributes.get("image") == self.seed_image)
            action = value.get("Action")
            require(isinstance(action, str))
            action = cast("str", action)
            if action in ("create", "start", "die", "destroy"):
                lifecycle.append(action)
            if action.startswith("exec_"):
                exec_id = digest(attributes.get("execID"))
                if action.startswith("exec_create: "):
                    command = action.removeprefix("exec_create: ")
                    _ = execution_kind(command)
                    require(exec_id not in executions)
                    executions[exec_id] = command
                elif action.startswith("exec_start: "):
                    require(exec_id in executions and exec_id not in started)
                    require(executions[exec_id] == action.removeprefix("exec_start: "))
                    started.add(exec_id)
                else:
                    require(action == "exec_die" and exec_id in started and exec_id not in statuses)
                    code = attributes.get("exitCode")
                    require(isinstance(code, str) and re.fullmatch(r"0|[1-9][0-9]{0,2}", code))
                    statuses[exec_id] = integer(int(cast("str", code)), 0, 255)
        require(lifecycle == ["create", "start", "die", "destroy"])
        require(executions.keys() == statuses.keys())
        return execution_sequence(
            [execution_kind(command) for command in executions.values()],
            [statuses[identifier] for identifier in executions],
            negative=negative,
        )

    def invoke(self) -> Result:
        """Require the real prepared-host public CLI, no core/helper entry or shim."""
        self.control.advance()
        self.wire.no_extra()
        self.control.checkpoint = "native"
        self.commands.observer = self.observe_actor
        self.observation_started = stage.utc()
        try:
            return self.commands.run(
                [
                    "release",
                    "backup",
                    "verify",
                    "--release-attempt",
                    "release." + cast("str", self.value["runId"]),
                    "--timeout-seconds",
                    "1200",
                    "--finalization-seconds",
                    "120",
                ],
                timeout=NATIVE,
                native=True,
            )
        finally:
            self.commands.observer = None

    def observe(self, result: Result) -> dict[str, object]:  # noqa: PLR0915 -- One fixed verdict.
        """Join actual native exit with private records and independent actor absence."""
        self.control.checkpoint = "native-exit"
        negative = self.value["scenario"] == "privacy-invalid"
        require(result.status == int(negative))
        self.control.checkpoint = "native-streams"
        require(
            result.output
            == (
                b"" if negative else b"Isolated restore verified and disposable database removed.\n"
            )
        )
        require(
            result.error
            == (
                b"Restore verification failed; inspect private restore evidence.\n"
                if negative
                else b""
            )
        )
        self.control.checkpoint = "native-record"
        new = [p for p in (STATE / "restores").iterdir() if p.name not in self.attempts_before]
        require(len(new) == 1)
        attempt = new[0]
        report = json_record(attempt / "restore.json", self.control, self.originals)
        report = exact(
            report,
            {"container", "containerId", "postgresImage", "cleanupPassed", "verified"}
            | ({"counts"} if "counts" in report else set()),
        )
        entries = {path.name for path in attempt.iterdir()}
        require(len(entries) <= 2 * MAX_NATIVE_COMMANDS + 2)
        logs: dict[int, set[str]] = {}
        for name in entries - {"restore.json", "outcome.json"}:
            match = re.fullmatch(r"([0-9]{3})\.(stdout|stderr)", name)
            require(match is not None)
            if match is not None:
                ordinal = integer(int(match[1]), 1, MAX_NATIVE_COMMANDS)
                logs.setdefault(ordinal, set()).add(match[2])
        require(logs and set(logs) == set(range(1, max(logs) + 1)))
        require(all(suffixes == {"stdout", "stderr"} for suffixes in logs.values()))
        self.control.checkpoint = "native-isolation"
        require(self.native_actor is not None)
        actor = cast("dict[str, object]", self.native_actor)
        require(report["containerId"] == actor["id"] and report["container"] == actor["name"])
        require(report["postgresImage"] == self.seed_image and report["cleanupPassed"] is True)
        require(report["verified"] is not negative)
        self.control.checkpoint = "cleanup"
        require(absent(attempt / "snapshot.dump", self.control))
        require(
            not self.commands.checked(
                ["ps", "--all", "--quiet", "--no-trunc", "--filter", "label=" + reference.LABEL]
            ).strip()
        )
        removed = self.commands.run(["inspect", cast("str", actor["id"])])
        require(removed.status == 1)
        self.control.checkpoint = "native-history"
        ready_count = self.history(cast("str", actor["id"]), negative=negative)
        ledger = self.retain(
            attempt / f"{ready_count + LEDGER_LOG_OFFSET:03}.stdout", 0o600, MAX_STREAM
        )
        require(ledger.data(MAX_STREAM).strip() == self.source_ledger)
        privacy = False
        if negative:
            self.control.checkpoint = "privacy-refusal"
            require(absent(attempt / "outcome.json", self.control))
            require(self.prior_freshness is not None)
            required(self.prior_freshness).check()
            for path in sorted(attempt.glob("*.stderr")):
                original = self.retain(path, 0o600, MAX_STREAM)
                privacy |= b"Restored role privacy differs" in original.data(MAX_STREAM)
            require(privacy)
        else:
            self.control.checkpoint = "freshness"
            require(self.prior_freshness is not None)
            current = (STATE / "restore-verified.timestamp").lstat()
            require(
                (current.st_dev, current.st_ino)
                != (
                    required(self.prior_freshness).before.st_dev,
                    required(self.prior_freshness).before.st_ino,
                )
            )
            outcome = json_record(attempt / "outcome.json", self.control, self.originals)
            require(
                isinstance(outcome.get("finishedAt"), str)
                and re.fullmatch(
                    r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z",
                    cast("str", outcome["finishedAt"]),
                )
            )
            fresh = json_record(STATE / "restore-verified.timestamp", self.control, self.originals)
            require(
                outcome
                == {
                    **report,
                    "backupSha256": self.archive.sha256 if self.archive else None,
                    "release": self.value["revision"],
                    "finishedAt": outcome["finishedAt"],
                }
            )
            require(fresh == {"evidence": str(attempt), **outcome})
        self.control.checkpoint = "lock"
        descriptor = os.open(WORK / "workload.lock", os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC)
        first: BaseException | None = None
        try:
            require(
                self.lock_before is not None
                and ops.stamp(os.fstat(descriptor)) == ops.stamp(self.lock_before)
            )
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
            fcntl.flock(descriptor, fcntl.LOCK_UN)
        except BaseException as error:  # noqa: BLE001 -- Attempt all known peer finalizers.
            first = error
        finally:
            first = ops.settle((lambda: os.close(descriptor),), first)
        if first is not None:
            raise first
        self.control.checkpoint = "source-recheck"
        require(self.sql(LEDGER_QUERY).strip() == self.source_ledger)
        require(self.counts() == self.source_counts)
        anchor = inspection(self.commands.checked(["inspect", ANCHOR]))
        require(len(anchor) == 1)
        require(dictionary(anchor[0]).get("Id") == self.seed_id)
        require(dictionary(anchor[0]).get("Image") == self.seed_image)
        for original in self.originals:
            original.check()
        for path, descriptor, before, names in self.namespaces:
            self.namespace_check(path, descriptor, before, names)
        require(self.archive is not None)
        return native_projection(
            {
                "containerId": actor["id"],
                "postgresImage": self.seed_image,
                "sourceUnchanged": True,
                "sourceLedgerAndCountsUnchanged": True,
                "snapshotAbsent": True,
                "labelAbsent": True,
                "isolationInspected": True,
                "actorRemoved": True,
                "freshnessMatched": not negative,
                "freshnessPreserved": negative,
                "lockReacquired": True,
                "verified": not negative,
                "cleanupPassed": True,
                "counts": report.get("counts"),
                "backupSha256": required(self.archive).sha256,
                "outcomePresent": not negative,
                "privacyRefusalObserved": privacy,
                "nativeRawExit": result.status,
                "engineVersion": self.value["engineVersion"],
                "postgresVersionMatched": True,
                "historyObserved": True,
                "restoreExecObserved": True,
                "amcheckExecObserved": not negative,
            },
            cast("str", self.value["scenario"]),
        )

    def finish(self) -> None:
        """Remove only the exact fixture-source CID; native leftovers remain negative."""
        actions: list[Callable[[], None]] = []
        if self.seed_id is not None:

            def remove() -> None:
                require(self.seed_id is not None)
                raw = inspection(self.commands.checked(["inspect", required(self.seed_id)]))
                require(len(raw) == 1)
                item = dictionary(raw[0])
                labels = cast(
                    "dict[str, object]", cast("dict[str, object]", item["Config"])["Labels"]
                )
                require(item["Id"] == self.seed_id and item["Image"] == self.seed_image)
                require(labels.get(FIXTURE_LABEL) == self.value["runId"])
                _ = self.commands.checked(["rm", "--force", "--volumes", required(self.seed_id)])
                require(
                    not self.commands.checked(
                        [
                            "ps",
                            "--all",
                            "--quiet",
                            "--no-trunc",
                            "--filter",
                            "id=" + required(self.seed_id),
                        ]
                    ).strip()
                )

            actions.append(remove)
        for original in self.originals:
            actions.extend((original.check, original.close))
        if self.prior_freshness is not None:
            actions.append(self.prior_freshness.close)
        for path, descriptor, before, names in self.namespaces:
            actions.extend(
                (
                    lambda path=path, descriptor=descriptor, before=before, names=names: (
                        self.namespace_check(path, descriptor, before, names)
                    ),
                    lambda descriptor=descriptor: os.close(descriptor),
                )
            )
        first = ops.settle(actions)
        if first is not None:
            raise first


def reference_id(value: str) -> str:
    """Validate a fixed CID before lending it to reference argv construction."""
    return digest(value)


def execute(control: Control, wire: Wire) -> dict[str, object]:
    """Preserve first failure while every known original/resource is finalized."""
    value = request(wire.line())
    control.configure(value)
    report: dict[str, object] = {
        "schemaVersion": 1,
        "scope": "RestoreVerificationGuest",
        "runId": value["runId"],
        "revision": value["revision"],
        "scenario": value["scenario"],
        "passed": False,
        "failure": "refused",
        "nativeRawExit": None,
        "nativePassed": None,
        "finalStatus": 0,
        "checkpoint": "input",
        "cleanupPassed": False,
        "native": None,
        "sourcePins": value["sourcePins"],
    }
    fixture = Fixture(control, value, wire)
    first: BaseException | None = None
    try:
        fixture.setup()
        wire.emit(
            {
                "schemaVersion": 1,
                "scope": "RestoreVerificationReady",
                "runId": value["runId"],
                "ready": True,
            }
        )
        go = exact(ops.object_json(wire.line()), {"schemaVersion", "runId", "go"})
        require(
            type(go["schemaVersion"]) is int
            and go["schemaVersion"] == 1
            and go["runId"] == value["runId"]
            and go["go"] is True
        )
        result = fixture.invoke()
        report["nativeRawExit"], report["nativePassed"] = result.status, result.status == 0
        report["native"] = fixture.observe(result)
        control.check()
        report["failure"] = None
    except BaseException as error:  # noqa: BLE001 -- All owners finalize, retaining first identity.
        first = error
        report["failure"] = classification(error, control.status)
    finally:
        report["checkpoint"] = control.checkpoint
        if fixture.commands.native_status is not None:
            report["nativeRawExit"] = fixture.commands.native_status
            report["nativePassed"] = fixture.commands.native_status == 0
        try:
            wire.no_extra()
            control.check()
        except BaseException as error:  # noqa: BLE001 -- Live-body refusal precedes widening.
            if first is None:
                report["failure"] = classification(error, control.status)
            first = first or error
        control.settling = True
        cleanup = ops.settle((fixture.finish,))
        first = first or cleanup
        report["cleanupPassed"] = cleanup is None
        if cleanup is not None and report["failure"] is None:
            report["failure"] = "cleanup"
        report["finalStatus"] = control.status
        report["passed"] = first is None and control.status == 0
        if report["passed"] is True:
            report["checkpoint"] = "complete"
    return report


def main() -> int:
    """Restore every process-global setting before the original final success fence."""
    control = Control(time.monotonic())
    mask = os.umask(0o077)
    handlers: dict[signal.Signals, object] = {}
    wire: Wire | None = None
    status = 1
    try:
        for number in (signal.SIGINT, signal.SIGTERM):
            handlers[number] = signal.signal(number, control.signal)
        wire = Wire(control)
        report = execute(control, wire)
        wire.emit(report)
        status = 0 if report["passed"] is True else 1
    except (OSError, ValueError, RuntimeError):
        _ = sys.stderr.write("Restore guest proof is unproven.\n")
    finally:
        actions: list[Callable[[], None]] = []
        if wire is not None:
            actions.append(wire.close)
        for number, handler in handlers.items():

            def restore(number: signal.Signals = number, handler: object = handler) -> None:
                _ = signal.signal(
                    number, cast("int | Callable[[int, object], None] | None", handler)
                )

            actions.append(restore)

        def restore_mask() -> None:
            _ = os.umask(mask)

        actions.append(restore_mask)
        if ops.settle(actions) is not None:
            status = 1
    try:
        control.check()
    except (OSError, ValueError):
        status = 1
    return control.status or status


if __name__ == "__main__":
    raise SystemExit(main())
