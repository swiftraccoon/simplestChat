"""Seal one authenticated per-run Ops artifact and execute its original read-only FD.

This standard-library receiver has no host policy, inventory, lease or engine
logic. The trusted controller supplies measured pins; this header is not an
independent signature or authentication capability. Successful exec ends Python
custody by kernel CLOEXEC retirement, not a checked-close claim.
"""

from __future__ import annotations

import time

ORIGIN = time.monotonic_ns()

# ruff: noqa: E402 -- Charge all remaining imports to the original remote clock.
import hashlib
import io
import json
import os
import re
import select
import signal
import stat
import sys
import tarfile
from contextlib import suppress
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, cast, final

if TYPE_CHECKING:
    from collections.abc import Callable, Sequence
    from types import FrameType
    from typing import IO, NoReturn

ROOT = Path("/srv/simplestchat-bench/artifacts")
OWNER_UID = 0
OWNER_GID = 0
MAX_VERSION = 32
MAX_VERSION_PART = 4294967295
NAMES = ("ops-release.json", "ops-selection.json", "simplestchat-ops")
MAX_HEADER = 65536
MAX_REQUEST = 512 * 1024
MAX_BINARY = 256 * 1024**2
MAX_TRANSFER = 281 * 1024**2
MEMBER_LIMITS = (65536, 16384, MAX_BINARY)
SAFE = 9007199254740991
CHUNK = 65536
NANOSECOND = 1000000000
HEADER_SECONDS = 60
STAGE_SECONDS = 300
MODE_PRIVATE = 0o600
MODE_DIRECTORY = 0o700
MODE_EXECUTABLE = 0o755
TAR_BLOCK = 512
TAR_END = 1024
TAR_PADDING = 10240
ENVIRONMENT = {"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LC_ALL": "C", "HOME": "/root"}
READ_FLAGS = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC
DIRECTORY_FLAGS = READ_FLAGS | os.O_DIRECTORY


@final
class BootstrapError(ValueError):
    """One closed failure code/status, never input text or an exception message."""

    def __init__(self, code: str, status: int = 1) -> None:
        """Retain only a source-selected code and the owner status."""
        super().__init__(code)
        self.code = code
        self.status = status


def fail(code: str, status: int = 1) -> NoReturn:
    """Raise a fixed source-selected refusal without reflecting input."""
    raise BootstrapError(code, status)


def require(value: object, code: str) -> None:
    """Refuse an unmet invariant without formatting private input."""
    if not value:
        fail(code)


@final
class Control:
    """One original clock and first known status span receive, checks and exec."""

    def __init__(self, origin: int) -> None:
        """Reserve the fixed stage bound, never a resettable timeout."""
        self.origin = origin
        self.end = origin + STAGE_SECONDS * NANOSECOND
        self.failure: BootstrapError | None = None

    def signal(self, number: int, _: object) -> None:
        """Latch INT/TERM without throwing into an acquired descriptor owner."""
        if self.failure is None:
            self.failure = BootstrapError(
                "interrupted" if number == signal.SIGINT else "terminated",
                130 if number == signal.SIGINT else 143,
            )

    def remember(self, error: BaseException) -> None:
        """Latch a body/close failure before a later finalizer can receive a signal."""
        if self.failure is None:
            self.failure = classify(error)

    def check(self, cutoff: int | None = None) -> None:
        """Observe the original end and signals before every bounded operation."""
        if self.failure is not None:
            raise self.failure
        now = time.monotonic_ns()
        require(self.origin > 0 and self.origin <= now, "unproven")
        if now >= min(self.end, self.end if cutoff is None else cutoff):
            self.failure = BootstrapError("deadline", 124)
            raise self.failure


@dataclass
class Descriptor:
    """Exactly one explicit close attempt; an after-effect failure is not retried."""

    fd: int

    def close(self) -> None:
        """Disarm before close so a reused descriptor can never be closed twice."""
        if self.fd >= 0:
            fd, self.fd = self.fd, -1
            try:
                os.close(fd)
            except OSError as error:
                failure = BootstrapError("close")
                raise failure from error


class Owners:
    """Keep the small receiver's acquired descriptors until every peer is settled."""

    def __init__(self) -> None:
        """Register ownership before any post-open validation can fail."""
        self.descriptors: list[Descriptor] = []
        self.control: Control | None = None

    def add(self, fd: int) -> Descriptor:
        """Adopt only the immediately preceding owned os.open result."""
        value = Descriptor(fd)
        self.descriptors.append(value)
        return value

    def close(self, keep: Descriptor | None = None) -> BaseException | None:
        """Attempt all acquired peers even after interruption or a close fault."""
        primary: BaseException | None = None
        for value in reversed(self.descriptors):
            if value is keep:
                continue
            try:
                value.close()
            except BaseException as error:  # noqa: BLE001 -- Finalize all peers on interrupts too.
                if self.control is not None:
                    self.control.remember(error)
                if primary is None:
                    primary = error
        return primary


def close_peer(close: Callable[[], None]) -> None:
    """Classify checked library-file close failures without leaking their messages."""
    try:
        close()
    except OSError as error:
        failure = BootstrapError("close")
        raise failure from error


def inventory(fd: int, control: Control) -> set[str]:
    """Stop at the first unselected name instead of allocating an unbounded listing."""
    control.check()
    entries = os.scandir(fd)
    primary: BaseException | None = None
    result: set[str] = set()
    try:
        for entry in entries:
            control.check()
            require(len(result) < len(NAMES) and entry.name in NAMES, "namespace")
            result.add(entry.name)
    except BaseException as error:
        primary = error
        control.remember(error)
        raise
    finally:
        try:
            close_peer(entries.close)
        except BaseException:
            if primary is None:
                raise
    control.check()
    return result


def stamp(value: os.stat_result) -> tuple[int, ...]:
    """Bind ownership, inode, mode, length and mutation facts, excluding atime."""
    return (
        value.st_dev,
        value.st_ino,
        value.st_mode,
        value.st_uid,
        value.st_gid,
        value.st_nlink,
        value.st_size,
        value.st_mtime_ns,
        value.st_ctime_ns,
    )


def binding(value: os.stat_result) -> tuple[int, ...]:
    """Ancestor sibling timestamps do not select the receiver's namespace."""
    return stamp(value)[:5]


def exact(value: object, keys: set[str]) -> dict[str, object]:
    """Require a closed object without implicit types or unknown fields."""
    require(isinstance(value, dict), "framing")
    result = cast("dict[str, object]", value)
    require(set(result) == keys, "framing")
    return result


def integer(value: object, maximum: int = SAFE) -> int:
    """Reject bools, nonpositive counts and noninteger numeric tokens."""
    require(type(value) is int and 1 <= value <= maximum, "framing")
    return cast("int", value)


def hexadecimal(value: object, length: int) -> str:
    """Accept only complete lowercase fixed-width transport declarations."""
    require(
        isinstance(value, str) and re.fullmatch(r"[a-f0-9]{" + str(length) + "}", value), "framing"
    )
    return cast("str", value)


@dataclass(frozen=True)
class Pin:
    """A size and digest remain data supplied by the controller's opaque owner."""

    sha256: str
    count: int

    @classmethod
    def parse(cls, value: object, maximum: int) -> Pin:
        """Bound counts before any corresponding read or allocation."""
        item = exact(value, {"sha256", "bytes"})
        return cls(hexadecimal(item["sha256"], 64), integer(item["bytes"], maximum))


@dataclass(frozen=True)
class Header:
    """Only framing pins and a validated outer run select the fixed stage name."""

    raw: str
    mode: str
    run: str
    transfer: Pin
    members: tuple[Pin, ...]

    @classmethod
    def parse(cls, data: bytes) -> Header:
        """Use the standard parser with duplicate and noninteger-token refusal."""
        require(0 < len(data) <= MAX_HEADER and data.endswith(b"\n"), "framing")
        require(data.count(b"\n") == 1 and b"\r" not in data, "framing")
        require(not data.startswith(b"\xef\xbb\xbf"), "framing")

        def pairs(items: list[tuple[str, object]]) -> dict[str, object]:
            result: dict[str, object] = {}
            for name, value in items:
                require(name not in result, "framing")
                result[name] = value
            return result

        def invalid(_: str) -> object:
            fail("framing")

        raw = data[:-1].decode("utf-8", errors="strict")
        value = exact(
            cast(
                "object",
                json.loads(
                    raw, object_pairs_hook=pairs, parse_float=invalid, parse_constant=invalid
                ),
            ),
            {
                "schemaVersion",
                "scope",
                "mode",
                "run",
                "selection",
                "artifactId",
                "transfer",
                "members",
                "request",
            },
        )
        require(type(value["schemaVersion"]) is int and value["schemaVersion"] == 1, "framing")
        require(
            value["scope"] == "OpsCapacityBootstrap" and value["mode"] in ("stage", "invoke"),
            "framing",
        )
        selection = exact(
            value["selection"],
            {
                "repository",
                "revision",
                "packageVersion",
                "protocolVersion",
                "target",
                "buildRunId",
                "runAttempt",
            },
        )
        repository = selection["repository"]
        require(
            isinstance(repository, str)
            and re.fullmatch(
                r"[A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9][A-Za-z0-9_.-]{0,99}", repository
            ),
            "framing",
        )
        _ = hexadecimal(selection["revision"], 40)
        version = selection["packageVersion"]
        require(
            isinstance(version, str)
            and len(version) <= MAX_VERSION
            and re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", version),
            "framing",
        )
        require(
            all(int(part) <= MAX_VERSION_PART for part in cast("str", version).split(".")),
            "framing",
        )
        require(
            type(selection["protocolVersion"]) is int
            and selection["protocolVersion"] == 1
            and selection["target"] == "x86_64-unknown-linux-musl",
            "framing",
        )
        for key in ("buildRunId", "runAttempt"):
            _ = integer(selection[key])
        _ = integer(value["artifactId"])
        _ = Pin.parse(value["request"], MAX_REQUEST)
        members = exact(value["members"], set(NAMES))
        return cls(
            raw,
            cast("str", value["mode"]),
            hexadecimal(value["run"], 32),
            Pin.parse(value["transfer"], MAX_TRANSFER),
            tuple(
                Pin.parse(members[name], maximum)
                for name, maximum in zip(NAMES, MEMBER_LIMITS, strict=True)
            ),
        )


def read_input(count: int, control: Control, cutoff: int | None = None) -> bytes:
    """Read at most the selected bytes, never buffered bytes from the next frame."""
    require(0 < count <= CHUNK, "bounds")
    while True:
        control.check(cutoff)
        now = time.monotonic_ns()
        end = min(control.end, control.end if cutoff is None else cutoff)
        ready, _, _ = select.select([0], [], [], min(0.1, max(0, (end - now) / NANOSECOND)))
        control.check(cutoff)
        if ready:
            result = os.read(0, count)
            control.check(cutoff)
            require(result, "framing")
            return result


def read_header(control: Control) -> Header:
    """Consume the first LF one byte at a time without request read-ahead."""
    data = bytearray()
    cutoff = control.origin + HEADER_SECONDS * NANOSECOND
    while len(data) < MAX_HEADER:
        data += read_input(1, control, cutoff)
        if data.endswith(b"\n"):
            result = Header.parse(bytes(data))
            if result.mode == "invoke":
                control.end = min(control.end, cutoff)
            control.check()
            return result
    fail("framing")


def regular(value: os.stat_result, mode: int, count: int) -> None:
    """Require a root-owned, single-link exact regular stage member."""
    require(
        stat.S_ISREG(value.st_mode)
        and stat.S_IMODE(value.st_mode) == mode
        and value.st_uid == OWNER_UID
        and value.st_gid == OWNER_GID
        and value.st_nlink == 1
        and value.st_size == count,
        "namespace",
    )


@dataclass(frozen=True)
class Directory:
    """A retained parent plus its original child-name binding."""

    descriptor: Descriptor
    before: os.stat_result
    parent: int | None
    name: str

    def check(self) -> None:
        """Check visible ancestors without selecting unrelated sibling timestamps."""
        require(binding(os.fstat(self.descriptor.fd)) == binding(self.before), "changed")
        visible = os.stat(self.name, dir_fd=self.parent, follow_symlinks=False)
        require(binding(visible) == binding(self.before), "changed")


def open_parent(owners: Owners) -> list[Directory]:
    """Open the canonical existing root-owned parents without following links."""
    require(ROOT.is_absolute(), "namespace")
    result: list[Directory] = []
    for name in ("/", *ROOT.parts[1:]):
        parent = result[-1].descriptor.fd if result else None
        descriptor = owners.add(os.open(name, DIRECTORY_FLAGS, dir_fd=parent))
        before = os.fstat(descriptor.fd)
        require(
            stat.S_ISDIR(before.st_mode)
            and before.st_uid == OWNER_UID
            and before.st_gid == OWNER_GID
            and not before.st_mode & 0o022,
            "namespace",
        )
        result.append(Directory(descriptor, before, parent, name))
    require(stat.S_IMODE(result[-1].before.st_mode) == MODE_DIRECTORY, "namespace")
    return result


def write_all(fd: int, data: bytes, control: Control) -> None:
    """Check each partial write and keep the original operation budget."""
    view = memoryview(data)
    while view:
        control.check()
        count = os.write(fd, view)
        require(count > 0, "io")
        view = view[count:]
        control.check()


def hash_fd(fd: int, pin: Pin, control: Control) -> None:
    """Rewind and hash the complete opened object, then restore kernel cursor0."""
    require(os.lseek(fd, 0, os.SEEK_SET) == 0, "changed")
    digest = hashlib.sha256()
    remaining = pin.count
    while remaining:
        control.check()
        chunk = os.read(fd, min(CHUNK, remaining))
        require(chunk, "changed")
        digest.update(chunk)
        remaining -= len(chunk)
    require(os.read(fd, 1) == b"" and digest.hexdigest() == pin.sha256, "changed")
    require(os.lseek(fd, 0, os.SEEK_SET) == 0, "changed")
    control.check()


@dataclass(frozen=True)
class Member:
    """An original read-only member and its complete final stamp."""

    descriptor: Descriptor
    name: str
    before: os.stat_result
    pin: Pin

    def check(self, parent: int, control: Control) -> None:
        """Recheck the opened inode and original visible name around complete hashing."""
        control.check()
        require(stamp(os.fstat(self.descriptor.fd)) == stamp(self.before), "changed")
        require(
            stamp(os.stat(self.name, dir_fd=parent, follow_symlinks=False)) == stamp(self.before),
            "changed",
        )
        hash_fd(self.descriptor.fd, self.pin, control)
        require(stamp(os.fstat(self.descriptor.fd)) == stamp(self.before), "changed")
        require(
            stamp(os.stat(self.name, dir_fd=parent, follow_symlinks=False)) == stamp(self.before),
            "changed",
        )


def copy_member(  # noqa: PLR0913 -- Explicit source, selected member, owner and original clock.
    source: IO[bytes], stage: int, name: str, pin: Pin, *, owners: Owners, control: Control
) -> Member:
    """Copy exclusively, verify through original readers, and close writers before chmod."""
    writer = owners.add(
        os.open(name, os.O_RDWR | os.O_CREAT | os.O_EXCL | READ_FLAGS, MODE_PRIVATE, dir_fd=stage)
    )
    os.fchmod(writer.fd, MODE_PRIVATE)
    remaining = pin.count
    digest = hashlib.sha256()
    while remaining:
        control.check()
        chunk = source.read(min(CHUNK, remaining))
        require(chunk, "framing")
        write_all(writer.fd, chunk, control)
        digest.update(chunk)
        remaining -= len(chunk)
    require(source.read(1) == b"" and digest.hexdigest() == pin.sha256, "identity")
    os.fsync(writer.fd)
    before = os.fstat(writer.fd)
    regular(before, MODE_PRIVATE, pin.count)
    reader = owners.add(os.open(name, READ_FLAGS, dir_fd=stage))
    require(stamp(os.fstat(reader.fd)) == stamp(before), "changed")
    hash_fd(reader.fd, pin, control)
    writer.close()
    require(stamp(os.fstat(reader.fd)) == stamp(before), "changed")
    if name == NAMES[-1]:
        os.fchmod(reader.fd, MODE_EXECUTABLE)
        changed = os.fstat(reader.fd)
        old, new = stamp(before), stamp(changed)
        require(
            old[:2] == new[:2]
            and new[2] == stat.S_IFREG | MODE_EXECUTABLE
            and old[3:8] == new[3:8],
            "changed",
        )
        before = changed
    result = Member(reader, name, before, pin)
    result.check(stage, control)
    return result


def preflight_tar(fd: int, header: Header, control: Control) -> None:
    """Bound physical USTAR headers before the mature parser sees metadata bodies."""
    require(os.lseek(fd, 0, os.SEEK_SET) == 0, "framing")
    offset = 0
    for name, pin in zip(NAMES, header.members, strict=True):
        control.check()
        raw = os.read(fd, TAR_BLOCK)
        require(len(raw) == TAR_BLOCK and raw[257:265] == b"ustar\x0000", "framing")
        item = tarfile.TarInfo.frombuf(raw, "utf-8", "strict")
        require(
            item.type == tarfile.REGTYPE
            and item.name == name
            and item.size == pin.count
            and item.mode == MODE_PRIVATE
            and item.uid == item.gid == item.mtime == 0
            and not item.linkname
            and item.sparse is None,
            "framing",
        )
        offset += TAR_BLOCK + ((pin.count + TAR_BLOCK - 1) // TAR_BLOCK) * TAR_BLOCK
        require(offset <= header.transfer.count, "framing")
        require(os.lseek(fd, offset, os.SEEK_SET) == offset, "framing")
    tail_size = header.transfer.count - offset
    require(TAR_END <= tail_size <= TAR_PADDING, "framing")
    tail = os.read(fd, tail_size + 1)
    require(len(tail) == tail_size and not any(tail), "framing")
    require(os.lseek(fd, 0, os.SEEK_SET) == 0, "framing")
    control.check()


def transfer(header: Header, parent: int, owners: Owners, control: Control) -> Descriptor:
    """Spool only the declared archive, leaving exact request bytes on stdin."""
    name = ".ops-transfer." + header.run
    spool = owners.add(
        os.open(name, os.O_RDWR | os.O_CREAT | os.O_EXCL | READ_FLAGS, MODE_PRIVATE, dir_fd=parent)
    )
    os.fchmod(spool.fd, MODE_PRIVATE)
    before = os.fstat(spool.fd)
    regular(before, MODE_PRIVATE, 0)
    require(stamp(os.stat(name, dir_fd=parent, follow_symlinks=False)) == stamp(before), "changed")
    os.unlink(name, dir_fd=parent)
    require(os.fstat(spool.fd).st_nlink == 0, "changed")
    remaining = header.transfer.count
    digest = hashlib.sha256()
    while remaining:
        chunk = read_input(min(CHUNK, remaining), control)
        write_all(spool.fd, chunk, control)
        remaining -= len(chunk)
        digest.update(chunk)
    require(digest.hexdigest() == header.transfer.sha256, "identity")
    os.fsync(spool.fd)
    hash_fd(spool.fd, header.transfer, control)
    preflight_tar(spool.fd, header, control)
    return spool


def receive(
    header: Header, stage: int, parent: int, owners: Owners, control: Control
) -> list[Member]:
    """Extract preflighted members while preserving the first body/finalizer failure."""
    spool = transfer(header, parent, owners, control)
    stream = io.FileIO(spool.fd, mode="rb", closefd=False)
    archive: tarfile.TarFile | None = None
    primary: BaseException | None = None
    result: list[Member] = []
    try:
        archive = tarfile.open(fileobj=stream, mode="r:")  # noqa: SIM115 -- Attempt all peer closes and preserve the primary failure.
        for index, item in enumerate(archive):
            require(index < len(NAMES) and item.name == NAMES[index], "framing")
            source = archive.extractfile(item)
            require(source is not None, "framing")
            source = cast("IO[bytes]", source)
            body_failure: BaseException | None = None
            try:
                result.append(
                    copy_member(
                        source,
                        stage,
                        item.name,
                        header.members[index],
                        owners=owners,
                        control=control,
                    )
                )
            except BaseException as error:
                body_failure = error
                control.remember(error)
                raise
            finally:
                try:
                    close_peer(source.close)
                except BaseException as error:
                    if body_failure is None:
                        control.remember(error)
                        raise
        require(len(result) == len(NAMES), "framing")
    except BaseException as error:
        primary = error
        control.remember(error)
        raise
    finally:
        for close in (*(() if archive is None else (archive.close,)), stream.close, spool.close):
            try:
                close_peer(close)
            except BaseException as error:  # noqa: BLE001 -- Attempt remaining peers on any fault.
                control.remember(error)
                if primary is None:
                    primary = error
        if primary is not None and sys.exception() is None:
            raise primary
    return result


def run(header: Header, control: Control, owners: Owners) -> None:
    """Admit the exact fixed namespace and perform the one original-FD exec."""
    owners.control = control
    control.check()
    directories = open_parent(owners)
    parent = directories[-1].descriptor.fd
    name = "ops-runtime." + header.run
    if header.mode == "stage":
        os.mkdir(name, MODE_DIRECTORY, dir_fd=parent)
    stage = owners.add(os.open(name, DIRECTORY_FLAGS, dir_fd=parent))
    before = os.fstat(stage.fd)
    require(
        stat.S_ISDIR(before.st_mode)
        and stat.S_IMODE(before.st_mode) == MODE_DIRECTORY
        and before.st_uid == OWNER_UID
        and before.st_gid == OWNER_GID,
        "namespace",
    )
    members: list[Member]
    if header.mode == "stage":
        require(not inventory(stage.fd, control), "namespace")
        members = receive(header, stage.fd, parent, owners, control)
        os.fsync(stage.fd)
        os.fsync(parent)
    else:
        members = []
        for member_name, pin in zip(NAMES, header.members, strict=True):
            descriptor = owners.add(os.open(member_name, READ_FLAGS, dir_fd=stage.fd))
            metadata = os.fstat(descriptor.fd)
            regular(
                metadata, MODE_EXECUTABLE if member_name == NAMES[-1] else MODE_PRIVATE, pin.count
            )
            members.append(Member(descriptor, member_name, metadata, pin))
    stage_stamp = stamp(os.fstat(stage.fd))
    require(inventory(stage.fd, control) == set(NAMES), "namespace")
    for member in members:
        member.check(stage.fd, control)
    for directory in directories:
        directory.check()
    require(
        stamp(os.fstat(stage.fd)) == stage_stamp
        and stamp(os.stat(name, dir_fd=parent, follow_symlinks=False)) == stage_stamp,
        "changed",
    )
    require(inventory(stage.fd, control) == set(NAMES), "namespace")
    executable = members[-1].descriptor
    require(not os.get_inheritable(executable.fd), "unproven")
    require(os.lseek(executable.fd, 0, os.SEEK_CUR) == 0, "changed")
    close_failure = owners.close(keep=executable)
    if close_failure is not None:
        raise close_failure
    control.check()
    require(stamp(os.fstat(executable.fd)) == stamp(members[-1].before), "changed")
    argv = [
        "simplestchat-ops",
        "capacity-vps",
        "serve",
        "--transport-json",
        header.raw,
        "--origin-monotonic-ns",
        str(control.origin),
    ]
    control.check()
    os.execve(executable.fd, argv, ENVIRONMENT)  # noqa: S606 -- Exact admitted original FD, no PATH.


def classify(error: BaseException) -> BootstrapError:
    """Map internal failures to the closed wire enum without reflecting error text."""
    if isinstance(error, BootstrapError):
        return error
    if isinstance(error, KeyboardInterrupt):
        return BootstrapError("interrupted", 130)
    if isinstance(error, (ValueError, UnicodeError, RecursionError, tarfile.TarError)):
        return BootstrapError("framing")
    if isinstance(error, FileNotFoundError):
        return BootstrapError("unavailable")
    if isinstance(error, FileExistsError):
        return BootstrapError("namespace")
    return BootstrapError("io")


def main(arguments: Sequence[str] | None = None) -> int:
    """Emit at most one fixed stderr line; successful execution never returns here."""
    control = Control(ORIGIN)
    owners = Owners()
    owners.control = control
    old: dict[
        signal.Signals, signal.Handlers | Callable[[int, FrameType | None], object] | int | None
    ] = {}
    primary: BootstrapError | None = None
    try:
        for number in (signal.SIGINT, signal.SIGTERM):
            old[number] = signal.signal(number, control.signal)
        if sys.argv[1:] if arguments is None else arguments:
            fail("arguments", 2)
        require(os.geteuid() == OWNER_UID and sys.platform == "linux", "platform")
        _ = os.umask(0o077)
        header = read_header(control)
        run(header, control, owners)
    except BaseException as error:  # noqa: BLE001 -- Closed diagnostics and complete peer finalization.
        primary = control.failure or classify(error)
        control.failure = primary
    finally:
        close_failure = owners.close()
        if primary is None and close_failure is not None:
            primary = BootstrapError("close")
            control.failure = primary
        for number, handler in old.items():
            try:
                _ = signal.signal(number, handler)
            except BaseException:  # noqa: BLE001 -- Every handler peer must be restored.
                if primary is None:
                    primary = BootstrapError("io")
    primary = primary or control.failure or BootstrapError("unproven")
    with suppress(OSError, ValueError):
        _, writable, _ = select.select([], [2], [], 0)
        if writable:
            _ = os.write(2, ("ops_capacity_" + primary.code + "\n").encode("ascii"))
    return primary.status


if __name__ == "__main__":
    sys.exit(main())
