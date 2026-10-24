"""Receive authenticated bytes and check one fixed Ops CLI inside its marked guest.

This fixture is controller code, never candidate code. It has no host, destination,
image, executable, root or command selector. Original pins are supplied by the
authenticated controller; candidate output cannot replace them.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import math
import os
import platform
import re
import signal
import stat
import subprocess
import sys
import tarfile
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, cast, final, override

if TYPE_CHECKING:
    from collections.abc import Callable, Sequence
    from typing import BinaryIO, NoReturn


ROOT = Path("/root/simplestchat-vm-fixture")
INPUT = ROOT / "ops-input"
LIBRARY = Path("/usr/local/libexec/simplestchat-public")
INSTALLED = LIBRARY / "simplestchat-ops"
MARKER = Path("/etc/simplestchat-vm-fixture")
NAMES = ("ops-release.json", "ops-selection.json", "simplestchat-ops")
MAX_BINARY = 256 * 1024**2
MAX_METADATA = 65536
MAX_SELECTION = 16384
MAX_TRANSFER = MAX_BINARY + MAX_METADATA + MAX_SELECTION + 16384
MAX_RECEIPT = 65536
SAFE = 9007199254740991
CHUNK = 65536
PRIVATE_MODE = 0o600
DIRECTORY_MODE = 0o700
EXECUTABLE_MODE = 0o755
HELPER_MODE = 0o644
TAR_BLOCK = 512
TAR_END = 1024
TAR_PADDING = 10240
MAX_SELECTION_TEXT = 128
MAX_EXECUTABLE_JSON = 4096
ELF_CLASS = 64
MAX_ARGUMENT_BYTES = 24 * 1024
MAX_HELP = 8192
UUID_VERSION = 4
DEADLINE_STATUS = 124
SIGNAL_STATUSES = (130, 143)
CASES = (
    ("source-help", 0),
    ("source-invalid-cli", 2),
    ("source-forbidden-root", 2),
    ("source-inspect", 0),
    ("source-pair", 0),
    ("install-foreign-bytes", 1),
    ("install-foreign-mode", 1),
    ("install-first", 0),
    ("install-repeat", 0),
    ("installed-help", 0),
    ("installed-inspect", 0),
    ("installed-pair", 0),
    ("preflight-ordinary", 0),
    ("preflight-base", 0),
    ("preflight-fetch", 0),
    ("preflight-force", 0),
    ("preflight-invalid-shape", 1),
    ("preflight-unknown-helper", 1),
    ("preflight-invalid-digest", 1),
    ("preflight-combined", 1),
    ("preflight-cli", 2),
    ("preflight-unprivileged", 1),
    ("preflight-mode", 1),
    ("preflight-hash", 1),
    ("release-manifest", 0),
    ("release-manifest-private", 1),
    ("installed-final-inspect", 0),
    ("installed-final-pair", 0),
)
BASE = frozenset(
    {
        "bounded_process.py",
        "release-public.py",
        "release_public.py",
        "release_artifact.py",
        "release_assets.py",
        "browser_assets.py",
        "release_json.py",
        "runtime_profile.py",
        "reboot-public.py",
        "reboot_public.py",
    }
)
FETCH = frozenset({"fetch-release.py", "release_fetch_receiver.py", "release_trust.py"})
FORCE = frozenset({"force_release.py"})


class OpsError(ValueError):
    """A closed fixture failure; private input and command text never enters it."""


def require(value: object, code: str = "ops_guest_unproven") -> None:
    """Reject an unproven boundary without formatting its input."""
    if not value:
        raise OpsError(code)


def object_json(data: bytes) -> dict[str, object]:
    """Parse bounded UTF-8 objects with unique keys and finite JSON numbers."""
    require(0 < len(data) <= MAX_RECEIPT and not data.startswith(b"\xef\xbb\xbf"))

    def pairs(items: list[tuple[str, object]]) -> dict[str, object]:
        value: dict[str, object] = {}
        for name, item in items:
            require(name not in value)
            value[name] = item
        return value

    def constant(_: str) -> object:
        message = "ops_guest_unproven"
        raise OpsError(message)

    raw = cast(
        "object", json.loads(data.decode("utf-8"), object_pairs_hook=pairs, parse_constant=constant)
    )
    require(isinstance(raw, dict))

    def finite(value: object) -> None:
        if isinstance(value, float):
            require(math.isfinite(value))
        elif isinstance(value, dict):
            for item in cast("dict[str, object]", value).values():
                finite(item)
        elif isinstance(value, list):
            for item in cast("list[object]", value):
                finite(item)

    finite(raw)
    return cast("dict[str, object]", raw)


def stamp(value: os.stat_result) -> tuple[int, ...]:
    """Retain full ownership, mode, identity and mutation facts, excluding atime."""
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


def directory(path: Path, owner: int = 0) -> int:
    """Open a private fixed directory; callers explicitly settle its descriptor."""
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        value = os.fstat(descriptor)
        require(
            stat.S_ISDIR(value.st_mode)
            and stat.S_IMODE(value.st_mode) == DIRECTORY_MODE
            and value.st_uid == owner
            and value.st_gid == os.getgid()
        )
    except BaseException:
        os.close(descriptor)
        raise
    else:
        return descriptor


def empty_cache() -> None:
    """Keep the isolated bytecode prefix empty; -B alone still permits cached reads."""
    descriptor = directory(ROOT / "python-cache")
    try:
        original = os.fstat(descriptor)
        require(not os.listdir(descriptor))  # noqa: PTH208 -- Inspect the admitted directory FD.
        require(
            stamp(os.fstat(descriptor)) == stamp(original)
            and stamp((ROOT / "python-cache").lstat()) == stamp(original)
        )
    finally:
        os.close(descriptor)


def digest_stream(source: BinaryIO, count: int, checkpoint: Callable[[], None]) -> str:
    """Consume exactly the selected bytes, clipping every read to its original bound."""
    digest = hashlib.sha256()
    left = count
    while left:
        checkpoint()
        chunk = source.read(min(CHUNK, left))
        require(bool(chunk))
        digest.update(chunk)
        left -= len(chunk)
    checkpoint()
    require(source.read(1) == b"")
    return digest.hexdigest()


def copy_stream(
    source: BinaryIO, output: BinaryIO, count: int, checkpoint: Callable[[], None]
) -> str:
    """Copy exactly the original count, checking writes and the final source EOF."""
    digest_value = hashlib.sha256()
    left = count
    while left:
        checkpoint()
        chunk = source.read(min(CHUNK, left))
        require(bool(chunk) and output.write(chunk) == len(chunk))
        digest_value.update(chunk)
        left -= len(chunk)
    require(source.read(1) == b"")
    output.flush()
    os.fsync(output.fileno())
    return digest_value.hexdigest()


def settle(
    actions: Sequence[Callable[[], None]], primary: BaseException | None = None
) -> BaseException | None:
    """Attempt every acquired peer, preserving the first failure even on interruption."""
    for action in actions:
        try:
            action()
        except BaseException as error:  # noqa: BLE001 -- Every peer must close after interrupts too.
            if primary is None:
                primary = error
    return primary


@dataclass
class Held:
    """One original read-only inode, never a newly adopted pathname baseline."""

    file: BinaryIO
    name: str
    metadata: os.stat_result
    sha256: str
    bytes: int

    def check(self, parent: int, checkpoint: Callable[[], None]) -> None:
        """Compare the original FD and visible leaf before and after whole-byte hashing."""
        checkpoint()
        require(stamp(os.fstat(self.file.fileno())) == stamp(self.metadata))
        visible = os.stat(self.name, dir_fd=parent, follow_symlinks=False)
        require(stamp(visible) == stamp(self.metadata))
        _ = self.file.seek(0)
        require(digest_stream(self.file, self.bytes, checkpoint) == self.sha256)
        require(stamp(os.fstat(self.file.fileno())) == stamp(self.metadata))
        require(
            stamp(os.stat(self.name, dir_fd=parent, follow_symlinks=False)) == stamp(self.metadata)
        )


@final
class Receiver:
    """Private fixture seam; the real CLI always chooses fixed INPUT and UID zero."""

    def __init__(self, root: Path, *, owner: int = 0) -> None:
        """Retain the original directory while independently materializing three files."""
        self.root = root
        self.parent = directory(root, owner)
        self.original = os.fstat(self.parent)
        self.owner = owner
        self.files: dict[str, Held] = {}

    def check(self) -> None:
        """Directory mutation times may change; identity and permissions may not."""
        current = os.fstat(self.parent)
        visible = self.root.lstat()
        require(
            stamp(current)[:5] == stamp(self.original)[:5]
            and stamp(visible)[:5] == stamp(self.original)[:5]
        )

    def member(
        self,
        item: tarfile.TarInfo,
        stream: BinaryIO,
        expected: tuple[str, int],
        checkpoint: Callable[[], None],
    ) -> None:
        """Copy through an exclusive writer, then keep only the same read-only inode."""
        self.check()
        name = item.name
        wanted_sha, wanted_bytes = expected
        require(name not in self.files and item.size == wanted_bytes)
        descriptor = os.open(
            name,
            os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
            PRIVATE_MODE,
            dir_fd=self.parent,
        )
        writer: BinaryIO | None = None
        reader: BinaryIO | None = None
        primary: BaseException | None = None
        try:
            os.fchmod(descriptor, PRIVATE_MODE)
            writer = os.fdopen(descriptor, "w+b")
            descriptor = -1
            require(copy_stream(stream, writer, wanted_bytes, checkpoint) == wanted_sha)
            original = os.fstat(writer.fileno())
            require(
                stat.S_ISREG(original.st_mode)
                and stat.S_IMODE(original.st_mode) == PRIVATE_MODE
                and original.st_uid == self.owner
                and original.st_gid == os.getgid()
                and original.st_nlink == 1
                and original.st_size == wanted_bytes
            )
            reader = os.fdopen(
                os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=self.parent), "rb"
            )
            require(
                stamp(os.fstat(reader.fileno())) == stamp(original)
                and stamp(os.stat(name, dir_fd=self.parent, follow_symlinks=False))
                == stamp(original)
            )
            writer.close()
            writer = None
            require(stamp(os.fstat(reader.fileno())) == stamp(original))
            if name == "simplestchat-ops":
                os.fchmod(reader.fileno(), EXECUTABLE_MODE)
                changed = os.fstat(reader.fileno())
                before, after = stamp(original), stamp(changed)
                require(
                    before[:2] == after[:2]
                    and after[2] == (stat.S_IFREG | EXECUTABLE_MODE)
                    and before[3:8] == after[3:8]
                )
                original = changed
            held = Held(reader, name, original, wanted_sha, wanted_bytes)
            held.check(self.parent, checkpoint)
            self.files[name] = held
            reader = None
        except BaseException as error:
            primary = error
            raise
        finally:
            actions = [file.close for file in (writer, reader) if file is not None]
            if descriptor >= 0:
                actions.append(lambda: os.close(descriptor))
            failure = settle(actions, primary)
            if primary is None and failure is not None:
                raise failure

    def extract(
        self, source: BinaryIO, expected: dict[str, tuple[str, int]], checkpoint: Callable[[], None]
    ) -> None:
        """Use the mature tar parser for exactly three flat regular USTAR members."""
        self.check()
        require(
            set(expected) == set(NAMES) and not os.listdir(self.parent)  # noqa: PTH208 -- The original directory FD is authority.
        )
        _ = source.seek(0)
        end = 0
        observed: list[str] = []
        with tarfile.open(fileobj=source, mode="r:") as archive:
            for item in archive:
                checkpoint()
                require(
                    len(observed) < len(NAMES)
                    and item.name == NAMES[len(observed)]
                    and item.type == tarfile.REGTYPE
                    and not item.pax_headers
                    and not item.linkname
                    and item.uid == 0
                    and item.gid == 0
                    and item.mode == PRIVATE_MODE
                    and item.offset == end
                    and item.offset_data == item.offset + 512
                )
                position = source.tell()
                _ = source.seek(item.offset + 257)
                require(source.read(6) == b"ustar\0")
                _ = source.seek(position)
                stream = archive.extractfile(item)
                require(stream is not None)
                with cast("BinaryIO", stream) as stream:
                    self.member(item, stream, expected[item.name], checkpoint)
                observed.append(item.name)
                end = item.offset_data + ((item.size + 511) // 512) * 512
        require(tuple(observed) == NAMES)
        _ = source.seek(end)
        tail = source.read(10241)
        require(TAR_END <= len(tail) <= TAR_PADDING and not any(tail) and source.read(1) == b"")
        self.check()
        require(
            set(os.listdir(self.parent)) == set(NAMES)  # noqa: PTH208 -- Check the original directory FD.
        )

    def close(self) -> None:
        """Attempt every original File and directory close even after the first error."""
        actions = [held.file.close for held in self.files.values()]
        failure = settle(actions)
        self.files.clear()
        if self.parent >= 0:
            descriptor = self.parent
            self.parent = -1
            failure = settle((lambda: os.close(descriptor),), failure)
        require(failure is None)


def exact(value: object, keys: set[str]) -> dict[str, object]:
    """Require an exact selected object; no candidate-provided extras are public."""
    require(isinstance(value, dict))
    result = cast("dict[str, object]", value)
    require(set(result) == keys)
    return result


def integer(value: object, maximum: int = SAFE, *, minimum: int = 1) -> int:
    """Reject booleans and noncanonical input before using a bounded count."""
    require(type(value) is int and minimum <= value <= maximum)
    return cast("int", value)


def digest(value: object) -> str:
    """Select only a complete lowercase SHA-256 spelling."""
    require(isinstance(value, str) and re.fullmatch(r"[a-f0-9]{64}", value))
    return cast("str", value)


def selection(data: bytes) -> dict[str, object]:
    """Validate the exact seven caller-selected source declarations."""
    require(len(data) <= MAX_SELECTION)
    value = exact(
        object_json(data),
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
    require(
        value["repository"] == "swiftraccoon/simplestChat"
        and value["target"] == "x86_64-unknown-linux-musl"
        and type(value["protocolVersion"]) is int
        and value["protocolVersion"] == 1
    )
    for key, pattern in (
        ("revision", r"[a-f0-9]{40}"),
        ("packageVersion", r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"),
    ):
        item = value[key]
        require(
            isinstance(item, str)
            and len(item) <= MAX_SELECTION_TEXT
            and re.fullmatch(pattern, item)
        )
    _ = integer(value["buildRunId"])
    _ = integer(value["runAttempt"])
    return value


def executable(data: bytes, sha256: str, count: int) -> dict[str, object]:
    """Compare all nineteen measured fields, without claiming candidate self-authentication."""
    require(len(data) <= MAX_EXECUTABLE_JSON)
    booleans = {
        "littleEndian",
        "noInterpreter",
        "noNeeded",
        "pieFlag",
        "bindNow",
        "nonExecutableStack",
        "noWritableExecutableLoad",
        "relroPresent",
        "noRpath",
        "noRunpath",
        "noTextRelocations",
        "noLoaderHooks",
    }
    value = exact(
        object_json(data),
        booleans
        | {
            "sha256",
            "bytes",
            "architecture",
            "elfClass",
            "fileType",
            "programHeaders",
            "neededCount",
        },
    )
    require(
        value["sha256"] == sha256
        and type(value["bytes"]) is int
        and value["bytes"] == count
        and value["architecture"] == "x86_64"
        and type(value["elfClass"]) is int
        and value["elfClass"] == ELF_CLASS
        and value["fileType"] in ("executable", "dynamic")
        and all(type(value[key]) is bool for key in booleans)
    )
    _ = integer(value["programHeaders"], 65535)
    _ = integer(value["neededCount"], 65535, minimum=0)
    require(
        value["littleEndian"] is True
        and value["noInterpreter"] is True
        and value["noNeeded"] is True
        and value["neededCount"] == 0
        and value["noLoaderHooks"] is True
    )
    return value


class Parser(argparse.ArgumentParser):
    """Keep untrusted argument spellings out of public argparse diagnostics."""

    @override
    def error(self, message: str) -> NoReturn:
        """Exit with a fixed syntax failure, without reflecting argv."""
        _ = message
        self.exit(2, "Ops guest arguments are invalid.\n")


@dataclass(frozen=True)
class Options:
    """Original controller pins, never reconstructed from candidate observations."""

    run_id: str
    selection_raw: bytes
    selected: dict[str, object]
    artifact_id: int
    pins: dict[str, tuple[str, int]]
    transfer_sha256: str
    transfer_bytes: int
    install_raw: bytes
    install_sha256: str
    measured: dict[str, object]

    @classmethod
    def parse(cls, arguments: Sequence[str]) -> Options:
        """Admit the fixed argv contract before any guest mutation or process."""
        require(sum(len(item.encode()) + 1 for item in arguments) <= MAX_ARGUMENT_BYTES)
        parser = Parser(add_help=False, allow_abbrev=False)
        for name in (
            "run-id",
            "selection-json",
            "artifact-id",
            "transfer-sha256",
            "transfer-bytes",
            "manifest-sha256",
            "manifest-bytes",
            "selection-sha256",
            "selection-bytes",
            "binary-sha256",
            "binary-bytes",
            "install-request-json",
            "install-request-sha256",
            "executable-json",
        ):
            _ = parser.add_argument("--" + name, required=True)
        parsed = vars(parser.parse_args(list(arguments)))

        def text(name: str) -> str:
            value = cast("object", parsed[name])
            require(isinstance(value, str))
            return cast("str", value)

        def number(name: str, maximum: int) -> int:
            value = text(name)
            require(re.fullmatch(r"[1-9][0-9]{0,15}", value))
            return integer(int(value), maximum)

        run_id, raw = text("run_id"), text("selection_json").encode()
        require(re.fullmatch(r"[a-f0-9]{32}", run_id))
        selected = selection(raw)
        pins = {
            name: (digest(text(prefix + "_sha256")), number(prefix + "_bytes", maximum))
            for name, prefix, maximum in zip(
                NAMES,
                ("manifest", "selection", "binary"),
                (MAX_METADATA, MAX_SELECTION, MAX_BINARY),
                strict=True,
            )
        }
        require((hashlib.sha256(raw).hexdigest(), len(raw)) == pins[NAMES[1]])
        install_raw = text("install_request_json").encode()
        require(0 < len(install_raw) <= MAX_SELECTION)
        install_sha = digest(text("install_request_sha256"))
        require(hashlib.sha256(install_raw).hexdigest() == install_sha)
        request = exact(
            object_json(install_raw), {"schemaVersion", "manifest", "selection", "binary"}
        )
        require(type(request["schemaVersion"]) is int and request["schemaVersion"] == 1)
        for key, name in zip(("manifest", "selection", "binary"), NAMES, strict=True):
            pin = exact(request[key], {"path", "sha256", "bytes"})
            require(
                pin["path"] == str(INPUT / name)
                and pin["sha256"] == pins[name][0]
                and type(pin["bytes"]) is int
                and pin["bytes"] == pins[name][1]
            )
        return cls(
            run_id,
            raw,
            selected,
            number("artifact_id", SAFE),
            pins,
            digest(text("transfer_sha256")),
            number("transfer_bytes", MAX_TRANSFER),
            install_raw,
            install_sha,
            executable(text("executable_json").encode(), *pins[NAMES[2]]),
        )

    def argv(self) -> list[str]:
        """Preserve original JSON bytes when sending the fixed guest invocation."""
        values = {
            "run-id": self.run_id,
            "selection-json": self.selection_raw.decode(),
            "artifact-id": str(self.artifact_id),
            "transfer-sha256": self.transfer_sha256,
            "transfer-bytes": str(self.transfer_bytes),
            "install-request-json": self.install_raw.decode(),
            "install-request-sha256": self.install_sha256,
            "executable-json": json.dumps(self.measured, separators=(",", ":")),
        }
        for name, prefix in zip(NAMES, ("manifest", "selection", "binary"), strict=True):
            values[prefix + "-sha256"] = self.pins[name][0]
            values[prefix + "-bytes"] = str(self.pins[name][1])
        return [part for key, value in values.items() for part in ("--" + key, value)]


@final
class Control:
    """Latch cancellation once; repeated signals cannot interrupt finalizers."""

    def __init__(self, seconds: float = 1680) -> None:
        """All guest case admissions share this one original local fallback clock."""
        self.started = time.monotonic()
        self.end = self.started + seconds
        self.status = 0
        self.settling = False

    def signal(self, signum: int, _: object) -> None:
        """Record the first INT/TERM without throwing into an acquired owner."""
        if self.status == 0:
            self.status = 130 if signum == signal.SIGINT else 143

    def check(self) -> None:
        """Reject late or cancelled work; cleanup itself is never cancelled."""
        now = time.monotonic()
        require(math.isfinite(now) and self.started <= now, "ops_guest_clock")
        if self.status and not self.settling:
            message = "ops_guest_cancelled"
            raise OpsError(message)
        if now >= self.end + (120 if self.settling else 0):
            self.status = self.status or DEADLINE_STATUS
            message = "ops_guest_deadline"
            raise OpsError(message)


def read_fixed(path: Path, maximum: int, mode: int) -> tuple[bytes, os.stat_result]:
    """Read one original nofollow root-owned regular leaf and recheck its full stamp."""
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    with os.fdopen(descriptor, "rb") as source:
        before = os.fstat(source.fileno())
        require(
            stat.S_ISREG(before.st_mode)
            and stat.S_IMODE(before.st_mode) == mode
            and before.st_uid == 0
            and before.st_gid == 0
            and before.st_nlink == 1
            and 0 < before.st_size <= maximum
            and stamp(path.lstat()) == stamp(before)
        )
        data = source.read(before.st_size + 1)
        require(
            len(data) == before.st_size
            and stamp(os.fstat(source.fileno())) == stamp(before)
            and stamp(path.lstat()) == stamp(before)
        )
    return data, before


def guard(options: Options) -> None:
    """Require the explicitly marked disposable Debian13 QEMU guest, before mutation."""
    require(os.geteuid() == 0 and platform.system() == "Linux" and platform.machine() == "x86_64")
    release = platform.freedesktop_os_release()
    require(release.get("ID") == "debian" and release.get("VERSION_ID", "").split(".")[0] == "13")
    require(Path("/sys/class/dmi/id/sys_vendor").read_bytes().strip() == b"QEMU")
    marker, _ = read_fixed(MARKER, 64, PRIVATE_MODE)
    require(marker == options.run_id.encode() + b"\n")
    for path in (ROOT, INPUT, ROOT / "python-cache"):
        descriptor = directory(path)
        os.close(descriptor)
    empty_cache()
    stage, _ = read_fixed(ROOT / "selection.json", 1024, PRIVATE_MODE)
    require(
        object_json(stage)
        == {"schemaVersion": 1, "runId": options.run_id, "revision": options.selected["revision"]}
    )
    require(
        not Path("/var/run/docker.sock").exists()
        and not INSTALLED.exists()
        and not INSTALLED.is_symlink()
    )


def receive(source: BinaryIO, options: Options, control: Control) -> tuple[BinaryIO, Receiver]:
    """Authenticate the whole transfer before its mature parser can see member metadata."""
    path = ROOT / "ops-transfer.tar"
    descriptor = os.open(
        path, os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, PRIVATE_MODE
    )
    writer: BinaryIO | None = None
    reader: BinaryIO | None = None
    receiver: Receiver | None = None
    primary: BaseException | None = None
    try:
        os.fchmod(descriptor, PRIVATE_MODE)
        writer = os.fdopen(descriptor, "w+b")
        descriptor = -1
        require(
            copy_stream(source, writer, options.transfer_bytes, control.check)
            == options.transfer_sha256
        )
        original = os.fstat(writer.fileno())
        require(
            stat.S_ISREG(original.st_mode)
            and stat.S_IMODE(original.st_mode) == PRIVATE_MODE
            and original.st_uid == 0
            and original.st_gid == 0
            and original.st_nlink == 1
            and original.st_size == options.transfer_bytes
        )
        reader = os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC), "rb")
        require(
            stamp(os.fstat(reader.fileno())) == stamp(original)
            and stamp(path.lstat()) == stamp(original)
        )
        writer.close()
        writer = None
        require(
            digest_stream(reader, options.transfer_bytes, control.check) == options.transfer_sha256
        )
        require(stamp(os.fstat(reader.fileno())) == stamp(original))
        receiver = Receiver(INPUT)
        receiver.extract(reader, options.pins, control.check)
        held = receiver.files[NAMES[1]]
        _ = held.file.seek(0)
        require(held.file.read(MAX_SELECTION + 1) == options.selection_raw)
        result, completed = reader, receiver
        reader, receiver = None, None
    except BaseException as error:
        primary = error
        raise
    else:
        return result, completed
    finally:
        actions = [file.close for file in (writer, reader, receiver) if file is not None]
        if descriptor >= 0:
            actions.append(lambda: os.close(descriptor))
        failure = settle(actions, primary)
        if primary is None and failure is not None:
            raise failure


def write_new(path: Path, data: bytes, mode: int = PRIVATE_MODE) -> os.stat_result:
    """Create only a new private fixture leaf, never replace an existing entry."""
    descriptor = os.open(
        path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, PRIVATE_MODE
    )
    with os.fdopen(descriptor, "wb") as output:
        require(output.write(data) == len(data))
        output.flush()
        os.fchmod(output.fileno(), mode)
        os.fsync(output.fileno())
        value = os.fstat(output.fileno())
        require(
            stamp(path.lstat()) == stamp(value)
            and value.st_uid == 0
            and value.st_gid == 0
            and stat.S_IMODE(value.st_mode) == mode
            and value.st_nlink == 1
        )
    return value


@dataclass(frozen=True)
class Snapshot:
    """A protected original source/helper file; no access time is used as authority."""

    path: Path
    metadata: os.stat_result
    sha256: str

    @classmethod
    def capture(cls, path: Path, maximum: int, mode: int) -> Snapshot:
        """Use the same read-bound metadata as the digest's initial witness."""
        data, metadata = read_fixed(path, maximum, mode)
        return cls(path, metadata, hashlib.sha256(data).hexdigest())

    def check(self) -> None:
        """Reject same-byte replacement, owner/mode changes and same-inode mutation."""
        data, value = read_fixed(
            self.path, self.metadata.st_size, stat.S_IMODE(self.metadata.st_mode)
        )
        require(
            stamp(value) == stamp(self.metadata) and hashlib.sha256(data).hexdigest() == self.sha256
        )


@final
class Runner:
    """Run only fixed authenticated-FD CLI cases; final VM retirement owns guest descendants.

    Normal per-case success establishes direct leader and bounded output-pipe
    completion. It does not claim whole-group retirement after reaping. On any
    exceptional path the original unreaped group receives checked stop attempts;
    uncertainty stays negative until the outer VM owner destroys the guest.
    """

    def __init__(self, control: Control, receiver: Receiver) -> None:
        """Bind the trusted staged helper only after the marked guest guard succeeds."""
        sys.path.insert(0, str(LIBRARY))
        import bounded_process  # noqa: PLC0415 -- Fixed staged root-owned helper after guest admission.

        self.process = bounded_process
        self.control = control
        self.receiver = receiver
        self.binary = receiver.files[NAMES[2]]
        self.records: list[dict[str, object]] = []
        self.protected = [
            Snapshot.capture(LIBRARY / name, 1024**2, HELPER_MODE)
            for name in sorted(BASE | FETCH | FORCE)
        ]
        self.protected.extend(
            Snapshot.capture(path, MAX_METADATA, PRIVATE_MODE)
            for path in (
                ROOT / "security_vm_ops_guest.py",
                ROOT / "release_preflight.py",
                ROOT / "selection.json",
                Path("/etc/simplestchat-public/images.json"),
            )
        )
        self.directories: dict[Path, os.stat_result] = {}
        private = {
            Path("/root"),
            ROOT,
            INPUT,
            ROOT / "python-cache",
            ROOT / "tmp",
            Path("/etc/simplestchat-public"),
            Path("/srv/simplestchat-public"),
            Path("/srv/simplestchat-public/releases"),
        }
        public = {
            Path("/"),
            Path("/usr"),
            Path("/usr/local"),
            LIBRARY.parent,
            LIBRARY,
            Path("/etc"),
            Path("/srv"),
        }
        for path in sorted(private | public):
            value = path.lstat()
            require(
                stat.S_ISDIR(value.st_mode)
                and value.st_uid == 0
                and value.st_gid == 0
                and stat.S_IMODE(value.st_mode)
                == (DIRECTORY_MODE if path in private else EXECUTABLE_MODE)
            )
            self.directories[path] = value
        self.installed: Held | None = None
        self.install_first: bool | None = None
        self.install_second: bool | None = None
        self.env = {
            "PATH": "/usr/bin:/bin",
            "HOME": str(ROOT),
            "TMPDIR": str(ROOT / "tmp"),
            "LANG": "C.UTF-8",
            "LC_ALL": "C.UTF-8",
        }

    def check(self) -> None:
        """Recheck immutable inputs and trusted assertions around every candidate process."""
        self.control.check()
        self.receiver.check()
        empty_cache()
        for path, before in self.directories.items():
            require(stamp(path.lstat())[:5] == stamp(before)[:5])
        for held in self.receiver.files.values():
            held.check(self.receiver.parent, self.control.check)
        for original in self.protected:
            original.check()
        if self.installed is not None:
            descriptor = os.open(
                LIBRARY, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
            )
            try:
                self.installed.check(descriptor, self.control.check)
            finally:
                os.close(descriptor)

    def healthy(self) -> bool:
        """Latch the original deadline or signal before the process pump can refuse work."""
        try:
            self.control.check()
        except OpsError:
            return False
        else:
            return True

    def execute(
        self, file: BinaryIO | None, arguments: Sequence[str], *, unprivileged: bool = False
    ) -> tuple[int, bytes, bytes]:
        """Pass only the retained FD; no candidate pathname or PATH lookup executes code."""
        self.control.check()
        remaining = min(120.0, self.control.end - time.monotonic())
        if remaining <= 0:
            self.control.status = self.control.status or DEADLINE_STATUS
            message = "ops_guest_deadline"
            raise OpsError(message)
        if file is None:
            argv = [
                "/usr/bin/python3",
                "-I",
                "-B",
                "-X",
                "pycache_prefix=" + str(ROOT / "python-cache"),
                str(ROOT / "release_preflight.py"),
                *arguments,
            ]
            executable_path, inherited = "/usr/bin/python3", ()
        else:
            argv = ["simplestchat-ops", *arguments]
            executable_path = "/proc/self/fd/" + str(file.fileno())
            inherited = (file.fileno(),)
        output, error = io.BytesIO(), io.BytesIO()
        child: subprocess.Popen[bytes] | None = None
        primary: BaseException | None = None
        result = 1
        try:
            self.control.check()
            empty_cache()
            self.control.check()
            child = subprocess.Popen(  # noqa: S603 -- Fixed argv and original authenticated read-only FD.
                argv,
                executable=executable_path,
                pass_fds=inherited,
                close_fds=True,
                cwd=ROOT,
                env=self.env,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
                user=65534 if unprivileged else None,
                group=65534 if unprivileged else None,
                extra_groups=[] if unprivileged else None,
            )
            result = self.process.pump(
                child,
                output,
                error,
                self.process.Limits(
                    timeout=remaining,
                    stdout=MAX_RECEIPT,
                    stderr=16384,
                    healthy=self.healthy,
                ),
            )
        except BaseException as original:  # noqa: BLE001 -- Retain the failure through every owned close.
            primary = original
            if child is not None:
                primary = settle((lambda: self.process.stop(child),), primary)
        finally:
            if child is not None:
                primary = settle(
                    tuple(
                        stream.close
                        for stream in (child.stdin, child.stdout, child.stderr)
                        if stream is not None
                    ),
                    primary,
                )
        if primary is not None:
            raise primary
        self.control.check()
        return result, output.getvalue(), error.getvalue()

    def case(
        self,
        name: str,
        arguments: Sequence[str],
        *,
        file: BinaryIO | None = None,
        unprivileged: bool = False,
    ) -> tuple[bytes, bytes]:
        """Record only a fixed case name/status, requiring exact order and known expected exit."""
        require(len(self.records) < len(CASES) and CASES[len(self.records)][0] == name)
        self.check()
        status, output, error = self.execute(
            file or self.binary.file, arguments, unprivileged=unprivileged
        )
        expected = CASES[len(self.records)][1]
        self.records.append(
            {"name": name, "exitStatus": max(0, min(255, status)), "passed": status == expected}
        )
        require(status == expected)
        self.check()
        return output, error

    def finish(self) -> None:
        """Close the installed owner; input owners remain with Receiver."""
        if self.installed is not None:
            self.installed.file.close()
            self.installed = None


def error_receipt(output: bytes, error: bytes, code: str, *, schema: bool = False) -> None:
    """Require the documented fixed failure, not merely any nonzero command."""
    expected: dict[str, object] = {"passed": False, "failure": code}
    if schema:
        expected["schemaVersion"] = 1
    value = exact(object_json(output), set(expected))
    require(
        not error
        and value["passed"] is False
        and (not schema or type(value["schemaVersion"]) is int)
        and value == expected
    )


def help_receipt(output: bytes, error: bytes) -> None:
    """Keep the maintained command surface visible without invoking a mutation route."""
    required = (
        b"Usage: simplestchat-ops release manifest --path PATH\n",
        b"simplestchat-ops release preflight [--helpers-json JSON]\n",
        b"simplestchat-ops distribution inspect-executable --path PATH\n",
        b"simplestchat-ops distribution install --request ABS --request-sha256 SHA\n",
        b"simplestchat-ops distribution verify-pair --manifest PATH --path PATH --selection PATH\n",
    )
    require(
        not error
        and output.startswith(required[0])
        and all(item in output for item in required)
        and output.count(b"Usage:") == 1
        and len(output) <= MAX_HELP
    )


def pair_receipt(
    output: bytes, error: bytes, options: Options, manifest: dict[str, object]
) -> None:
    """Require full measured facts and counts independently derived from original metadata."""
    require(not error)
    value = exact(object_json(output), {"manifestSha256", "manifestBytes", "claims", "executable"})
    require(
        value["manifestSha256"] == options.pins[NAMES[0]][0]
        and type(value["manifestBytes"]) is int
        and value["manifestBytes"] == options.pins[NAMES[0]][1]
        and executable(json.dumps(value["executable"]).encode(), *options.pins[NAMES[2]])
        == options.measured
    )
    inputs = cast("dict[str, object]", manifest["inputDigests"])
    evidence = cast("dict[str, object]", manifest["evidenceDigests"])
    claims = exact(
        value["claims"],
        set(options.selected)
        | {"schemaVersion", "artifactType", "inputDigestCount", "evidenceDigestCount"},
    )
    require(
        type(claims["schemaVersion"]) is int
        and type(claims["inputDigestCount"]) is int
        and type(claims["evidenceDigestCount"]) is int
    )
    require(
        selection(json.dumps({key: claims[key] for key in options.selected}).encode())
        == options.selected
    )
    require(
        claims
        == {
            "schemaVersion": 1,
            "artifactType": "simplestchat-ops",
            **options.selected,
            "inputDigestCount": len(inputs),
            "evidenceDigestCount": len(evidence),
        }
    )


def install_receipt(output: bytes, error: bytes, options: Options, *, changed: bool) -> None:
    """Require the exact installer projection, correlated to all three original pins."""
    expected: dict[str, object] = {"schemaVersion": 1, "passed": True, "changed": changed}
    for name, prefix in zip(NAMES, ("manifest", "selection", "binary"), strict=True):
        expected[prefix + "Sha256"], expected[prefix + "Bytes"] = options.pins[name]
    value = exact(object_json(output), set(expected))
    require(
        not error
        and type(value["schemaVersion"]) is int
        and value["passed"] is True
        and value["changed"] is changed
        and all(
            type(value[prefix + "Bytes"]) is int for prefix in ("manifest", "selection", "binary")
        )
        and value == expected
    )


def preflight_projection(data: bytes) -> dict[str, object]:
    """Validate each independent UUID4 run ID, then normalize that field alone."""
    value = object_json(data)
    if "runId" in value:
        value = exact(value, {"schemaVersion", "runId", "prepared"})
        run_id = value["runId"]
        require(isinstance(run_id, str) and re.fullmatch(r"[a-f0-9]{32}", run_id))
        parsed = uuid.UUID(hex=cast("str", run_id))
        require(
            parsed.version == UUID_VERSION
            and parsed.variant == uuid.RFC_4122
            and type(value["schemaVersion"]) is int
            and value["schemaVersion"] == 1
            and type(value["prepared"]) is bool
        )
        return {**value, "runId": "<independent-uuid4>"}
    value = exact(value, {"schemaVersion", "passed", "failure"})
    require(
        type(value["schemaVersion"]) is int
        and value["schemaVersion"] == 1
        and value["passed"] is False
        and isinstance(value["failure"], str)
    )
    return value


def preflight_case(runner: Runner, name: str, helpers: object) -> None:
    """Execute the actual unchanged Python oracle and native code independently."""
    raw = json.dumps(helpers, separators=(",", ":"))
    output, error = runner.case(name, ["release", "preflight", "--helpers-json", raw])
    status, expected, oracle_error = runner.execute(None, [raw])
    require(
        status == CASES[len(runner.records) - 1][1]
        and not error
        and not oracle_error
        and preflight_projection(output) == preflight_projection(expected)
    )
    runner.check()


def remove_owned(path: Path, original: os.stat_result) -> None:
    """Unlink only this fixture's still-visible inode; never recover by adopting a replacement."""
    require(stamp(path.lstat()) == stamp(original))
    path.unlink()


def foreign_install(runner: Runner, options: Options, name: str, *, wrong_mode: bool) -> None:
    """Keep byte and mode refusals independent; only exact owned fixture files are removed."""
    if wrong_mode:
        source = runner.binary.file
        _ = source.seek(0)
        descriptor = os.open(
            INSTALLED, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, PRIVATE_MODE
        )
        with os.fdopen(descriptor, "wb") as output:
            while chunk := source.read(CHUNK):
                runner.control.check()
                require(output.write(chunk) == len(chunk))
            output.flush()
            os.fchmod(output.fileno(), HELPER_MODE)
            os.fsync(output.fileno())
    else:
        _ = write_new(INSTALLED, b"owned foreign fixture\n", EXECUTABLE_MODE)
    original = Snapshot.capture(
        INSTALLED, MAX_BINARY, HELPER_MODE if wrong_mode else EXECUTABLE_MODE
    )
    try:
        output, error = runner.case(
            name,
            [
                "distribution",
                "install",
                "--request",
                str(ROOT / "install-request.json"),
                "--request-sha256",
                options.install_sha256,
            ],
        )
        error_receipt(output, error, "ops_install_unproven", schema=True)
        original.check()
    finally:
        remove_owned(INSTALLED, original.metadata)


def retain_installed(options: Options) -> Held:
    """Keep the published installed inode read-only and compare bytes to controller authority."""
    file = os.fdopen(os.open(INSTALLED, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC), "rb")
    try:
        before = os.fstat(file.fileno())
        require(
            stat.S_ISREG(before.st_mode)
            and stat.S_IMODE(before.st_mode) == EXECUTABLE_MODE
            and before.st_uid == 0
            and before.st_gid == 0
            and before.st_nlink == 1
            and before.st_size == options.pins[NAMES[2]][1]
            and stamp(INSTALLED.lstat()) == stamp(before)
        )
        require(
            digest_stream(file, before.st_size, lambda: None) == options.pins[NAMES[2]][0]
            and stamp(os.fstat(file.fileno())) == stamp(before)
        )
        return Held(file, INSTALLED.name, before, options.pins[NAMES[2]][0], before.st_size)
    except BaseException:
        file.close()
        raise


def helper_mutation(runner: Runner, *, change_bytes: bool) -> None:
    """Alter one owned nonexecuted helper and restore exact bytes/mode under its same inode."""
    name = "preflight-hash" if change_bytes else "preflight-mode"
    path = LIBRARY / "bounded_process.py"
    index = next(i for i, item in enumerate(runner.protected) if item.path == path)
    original = runner.protected.pop(index)
    data, before = read_fixed(path, 1024**2, HELPER_MODE)
    fd = os.open(path, os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        require(stamp(os.fstat(fd)) == stamp(before))
        if change_bytes:
            require(os.pwrite(fd, b"#" if data[:1] != b"#" else b"!", 0) == 1)
            os.fsync(fd)
        else:
            os.fchmod(fd, PRIVATE_MODE)
        helpers = {
            item.path.name: item.sha256 for item in runner.protected if item.path.name in BASE
        }
        helpers[path.name] = original.sha256
        preflight_case(runner, name, helpers)
    finally:
        try:
            require(os.fstat(fd).st_ino == before.st_ino and path.lstat().st_ino == before.st_ino)
            require(os.pwrite(fd, data, 0) == len(data))
            os.fchmod(fd, HELPER_MODE)
            os.fsync(fd)
        finally:
            os.close(fd)
        restored = Snapshot.capture(path, 1024**2, HELPER_MODE)
        require(restored.sha256 == original.sha256 and restored.metadata.st_ino == before.st_ino)
        runner.protected.insert(index, restored)


def source_cases(
    runner: Runner, options: Options, manifest: dict[str, object]
) -> tuple[list[str], list[str]]:
    """Check the selected source and both refusal fixtures before installing it."""
    pair = [
        "distribution",
        "verify-pair",
        "--manifest",
        str(INPUT / NAMES[0]),
        "--path",
        str(INPUT / NAMES[2]),
        "--selection",
        str(INPUT / NAMES[1]),
    ]
    inspect = ["distribution", "inspect-executable", "--path", str(INPUT / NAMES[2])]
    help_receipt(*runner.case("source-help", ["--help"]))
    for name, args in (
        ("source-invalid-cli", ["--not-a-command"]),
        (
            "source-forbidden-root",
            ["release", "assets", "inspect", "--root", str(ROOT / "forbidden-root")],
        ),
    ):
        output, error = runner.case(name, args)
        require(not output and error.startswith(b"Usage: simplestchat-ops "))
    output, error = runner.case("source-inspect", inspect)
    require(not error and executable(output, *options.pins[NAMES[2]]) == options.measured)
    pair_receipt(*runner.case("source-pair", pair), options, manifest)
    foreign_install(runner, options, "install-foreign-bytes", wrong_mode=False)
    foreign_install(runner, options, "install-foreign-mode", wrong_mode=True)
    return inspect, pair


def install_cases(runner: Runner, options: Options) -> BinaryIO:
    """Require first publication and an exact same-inode idempotent repeat."""
    install = [
        "distribution",
        "install",
        "--request",
        str(ROOT / "install-request.json"),
        "--request-sha256",
        options.install_sha256,
    ]
    install_receipt(*runner.case("install-first", install), options, changed=True)
    runner.install_first = True
    runner.installed = retain_installed(options)
    install_receipt(*runner.case("install-repeat", install), options, changed=False)
    runner.install_second = False
    return runner.installed.file


def preflight_cases(runner: Runner) -> None:
    """Compare the maintained preflight and its independent Python reference."""
    hashes = {
        item.path.name: item.sha256
        for item in runner.protected
        if item.path.name in BASE | FETCH | FORCE
    }
    preflight_case(runner, "preflight-ordinary", {})
    preflight_case(runner, "preflight-base", {key: hashes[key] for key in sorted(BASE)})
    preflight_case(runner, "preflight-fetch", {key: hashes[key] for key in sorted(BASE | FETCH)})
    preflight_case(runner, "preflight-force", {key: hashes[key] for key in sorted(BASE | FORCE)})
    preflight_case(runner, "preflight-invalid-shape", [])
    preflight_case(runner, "preflight-unknown-helper", {"unknown.py": "0" * 64})
    invalid = {key: hashes[key] for key in sorted(BASE)}
    invalid["bounded_process.py"] = "invalid"
    preflight_case(runner, "preflight-invalid-digest", invalid)
    preflight_case(runner, "preflight-combined", hashes)
    output, error = runner.case("preflight-cli", ["release", "preflight", "--helpers-json"])
    require(not output and error.startswith(b"Usage: simplestchat-ops "))
    output, error = runner.case(
        "preflight-unprivileged", ["release", "preflight"], unprivileged=True
    )
    error_receipt(output, error, "root_required", schema=True)
    helper_mutation(runner, change_bytes=False)
    helper_mutation(runner, change_bytes=True)


def manifest_cases(runner: Runner, options: Options) -> None:
    """Check a valid server manifest and refuse a private-field mutation."""
    revision = cast("str", options.selected["revision"])
    value = {
        "schemaVersion": 1,
        "revision": revision,
        "platform": "linux/amd64",
        "archiveSha256": "0" * 64,
        "imageTag": "simplestchat-release/production:" + revision,
        "migrations": {"1": "a" * 96},
        "createdAt": "2026-10-23T00:00:00Z",
    }
    raw = json.dumps(value, separators=(",", ":")).encode()
    manifest_path = ROOT / "release-manifest.json"
    _ = write_new(manifest_path, raw)
    output, error = runner.case(
        "release-manifest", ["release", "manifest", "--path", str(manifest_path)]
    )
    require(
        not error
        and exact(object_json(output), {"manifest", "manifestSha256", "manifestBytes"})
        == {
            "manifest": value,
            "manifestSha256": hashlib.sha256(raw).hexdigest(),
            "manifestBytes": len(raw),
        }
    )
    returned = object_json(output)
    require(
        type(returned["manifestBytes"]) is int
        and type(cast("dict[str, object]", returned["manifest"])["schemaVersion"]) is int
    )
    private = ROOT / "release-manifest-private.json"
    _ = write_new(private, json.dumps({**value, "private": "must-not-be-projected"}).encode())
    error_receipt(
        *runner.case("release-manifest-private", ["release", "manifest", "--path", str(private)]),
        "invalid_release_manifest",
    )


def exercise(runner: Runner, options: Options) -> None:
    """Complete all twenty-eight cases under the original guest owner."""
    manifest_file = runner.receiver.files[NAMES[0]].file
    _ = manifest_file.seek(0)
    manifest = object_json(manifest_file.read(MAX_METADATA + 1))
    require(
        all(manifest[key] == value for key, value in options.selected.items())
        and isinstance(manifest["inputDigests"], dict)
        and isinstance(manifest["evidenceDigests"], dict)
    )
    _ = write_new(ROOT / "install-request.json", options.install_raw)
    runner.protected.append(
        Snapshot.capture(ROOT / "install-request.json", MAX_SELECTION, PRIVATE_MODE)
    )
    inspect, pair = source_cases(runner, options, manifest)
    installed = install_cases(runner, options)
    help_receipt(*runner.case("installed-help", ["--help"], file=installed))
    inspect[-1], pair[5] = str(INSTALLED), str(INSTALLED)
    output, error = runner.case("installed-inspect", inspect, file=installed)
    require(not error and executable(output, *options.pins[NAMES[2]]) == options.measured)
    pair_receipt(*runner.case("installed-pair", pair, file=installed), options, manifest)
    preflight_cases(runner)
    manifest_cases(runner, options)
    output, error = runner.case("installed-final-inspect", inspect, file=installed)
    require(not error and executable(output, *options.pins[NAMES[2]]) == options.measured)
    pair_receipt(*runner.case("installed-final-pair", pair, file=installed), options, manifest)
    require(len(runner.records) == len(CASES))


def run(options: Options, source: BinaryIO, control: Control) -> dict[str, object]:
    """Settle every input and installed owner before returning even a negative guest observation."""
    report: dict[str, object] = {
        "schemaVersion": 1,
        "scope": "OpsDebianGuestReceipt",
        "runId": options.run_id,
        "cases": [],
        "install": {"firstChanged": None, "secondChanged": None},
        "passed": False,
        "failure": "guard",
    }
    transfer: BinaryIO | None = None
    receiver: Receiver | None = None
    runner: Runner | None = None
    primary: BaseException | None = None
    try:
        guard(options)
        report["failure"] = "transfer"
        transfer, receiver = receive(source, options, control)
        runner = Runner(control, receiver)
        report["cases"] = runner.records
        report["failure"] = "cases"
        exercise(runner, options)
        runner.check()
        report["passed"], report["failure"] = True, None
    except BaseException as error:  # noqa: BLE001 -- Finalize every acquired owner before any return.
        primary = error
        control.status = control.status or 1
    finally:
        control.settling = True
        if runner is not None:
            report["install"] = {
                "firstChanged": runner.install_first,
                "secondChanged": runner.install_second,
            }
        actions = [
            close
            for close in (
                runner.finish if runner is not None else None,
                receiver.close if receiver is not None else None,
                transfer.close if transfer is not None else None,
            )
            if close is not None
        ]
        completed = settle((*actions, control.check), primary)
        if completed is not None and primary is None:
            report["failure"] = "cleanup"
        if completed is not None or control.status:
            report["passed"] = False
            if control.status == DEADLINE_STATUS or control.status in SIGNAL_STATUSES:
                report["failure"] = "deadline" if control.status == DEADLINE_STATUS else "cancelled"
    return report


def main(arguments: Sequence[str] | None = None) -> int:
    """Emit only the fixed bounded observation; detailed candidate output is never public."""
    control = Control()
    old = {
        number: signal.signal(number, control.signal) for number in (signal.SIGINT, signal.SIGTERM)
    }
    try:
        options = Options.parse(sys.argv[1:] if arguments is None else arguments)
        report = run(options, sys.stdin.buffer, control)
        output = json.dumps(report, separators=(",", ":")).encode() + b"\n"
        require(len(output) <= MAX_RECEIPT)
        _ = sys.stdout.buffer.write(output)
        _ = sys.stdout.buffer.flush()
        if report["passed"] is True:
            control.check()
        return control.status or (0 if report["passed"] is True else 1)
    except (OSError, ValueError, RecursionError):
        _ = sys.stderr.write("Ops guest validation is unproven.\n")
        return control.status or 1
    finally:
        for number, handler in old.items():
            _ = signal.signal(number, handler)


if __name__ == "__main__":
    sys.exit(main())
