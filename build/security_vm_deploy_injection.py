#!/usr/bin/python3 -I
"""Fixed disposable-guest inspection barrier; never an operational Docker replacement.

Only the exact native container-inspection request can use the private parent
service. Other arguments exec the original retained Docker FD unchanged. The
parent owns real inspection and the deliberate exact-ID stop. No Docker child
is created in client mode, and no response is fabricated by this helper.
"""

from __future__ import annotations

import fcntl
import hashlib
import json
import math
import os
import re
import signal
import socket
import stat
import struct
import subprocess
import sys
import time
from contextlib import suppress
from pathlib import Path
from typing import TYPE_CHECKING, NoReturn, cast, final

if TYPE_CHECKING:
    from collections.abc import Callable

ROOT = Path("/root/simplestchat-vm-fixture")
CONFIG = ROOT / "deploy-injection-config.json"
SOCKET = ROOT / "deploy-injection.sock"
ALIAS = ROOT / "deploy-injection-original-docker"
HELPER = ROOT / "security_vm_deploy_injection.py"
RECEIPT = ROOT / "deploy-injection-receipt.json"
LIBRARY = Path("/usr/local/libexec/simplestchat-public")
INSTALLED = LIBRARY / "simplestchat-ops"
MARKER = Path("/etc/simplestchat-vm-fixture")
DOCKER = Path("/usr/bin/docker")
MOUNT = "/usr/bin/mount"
FRAME = 16384
STREAM = 2 * 1024**2
TOOL = 320 * 1024**2
BINARY = 256 * 1024**2
CHUNK = 65536
SETTLEMENT = 10.0
REAP = 5.0
MARKER_BYTES = 33
STAMP_FIELDS = 9
MOUNT_FIELDS = 6
ENTER_ARGUMENTS = 3
STDERR_DESCRIPTOR = 2
PRIVATE = 0o600
EXECUTABLE = 0o755
FAILURE = b"Inspection barrier refused.\n"
NATIVE_FORMAT = (
    '{"id":{{json .Id}},"image":{{json .Image}},"state":{{json .State}},'
    '"restarts":{{.RestartCount}},'
    '"configHash":{{json (index .Config.Labels "com.docker.compose.config-hash")}}}'
)
ENV = {"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LC_ALL": "C", "HOME": str(ROOT)}
DOCKER_ENV = {"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LC_ALL": "C"}
_observed_signal = 0
_entry_descriptors: list[int] = []


def require(value: object) -> None:
    """Refuse without interpolating paths, data, exception text or daemon output."""
    if not value:
        message = "inspection_barrier_refused"
        raise ValueError(message)


def bootstrap() -> None:
    """Admit fixed import roots in the isolated interpreter before project imports."""
    require(os.geteuid() == 0 and sys.platform == "linux")
    descriptors: list[int] = []
    first: BaseException | None = None
    try:
        for path, mode in ((ROOT, 0o700), (LIBRARY, 0o755), (ROOT / "python-cache", 0o700)):
            require(path.resolve(strict=True) == path)
            descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
            descriptors.append(descriptor)
            metadata = os.fstat(descriptor)
            require(
                stat.S_ISDIR(metadata.st_mode)
                and metadata.st_uid == metadata.st_gid == 0
                and stat.S_IMODE(metadata.st_mode) == mode
            )
        descriptor = os.open(MARKER, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
        descriptors.append(descriptor)
        metadata = os.fstat(descriptor)
        require(
            stat.S_ISREG(metadata.st_mode)
            and metadata.st_uid == metadata.st_gid == 0
            and stat.S_IMODE(metadata.st_mode) == PRIVATE
            and metadata.st_nlink == 1
            and metadata.st_size == MARKER_BYTES
        )
        require(re.fullmatch(rb"[a-f0-9]{32}\n", os.read(descriptor, MARKER_BYTES + 1)) is not None)
    except BaseException as error:  # noqa: BLE001 -- Close all admissions before import refusal.
        first = error
    for descriptor in reversed(descriptors):
        try:
            os.close(descriptor)
        except BaseException as error:  # noqa: BLE001 -- Every admitted descriptor is attempted.
            first = first or error
    if first is not None:
        raise first
    require(not any((ROOT / "python-cache").iterdir()))
    sys.dont_write_bytecode = True
    sys.pycache_prefix = str(ROOT / "python-cache")
    sys.path.insert(0, str(LIBRARY))
    sys.path.insert(0, str(ROOT))


def refuse_bootstrap() -> NoReturn:
    """Close a known inherited entry FD even if bootstrap or project imports fail."""
    for descriptor in _entry_descriptors[:]:
        _entry_descriptors.remove(descriptor)
        # A bootstrap refusal is already primary; every known close is attempted.
        with suppress(OSError):
            os.close(descriptor)
    try:
        require(sys.stderr.buffer.write(FAILURE) == len(FAILURE))
        _ = sys.stderr.buffer.flush()
    except (OSError, ValueError):
        pass
    raise SystemExit(1) from None


if __name__ == "__main__":
    try:
        if sys.argv[1:2] == ["--enter"]:
            require(len(sys.argv) == ENTER_ARGUMENTS + 1)
            require(re.fullmatch(r"[0-9]+", sys.argv[2]) is not None)
            _descriptor = int(sys.argv[2])
            require(_descriptor > STDERR_DESCRIPTOR)
            _entry_descriptors.append(_descriptor)
            _ = os.fstat(_descriptor)
        bootstrap()
    except BaseException:  # noqa: BLE001 -- Bootstrap exposes only the fixed refusal.
        refuse_bootstrap()

try:
    import security_vm_ops_guest as ops_guest
    from release_json import (
        JsonObject,
        JsonValue,
        array_value,
        decode_json,
        integer_value,
        object_value,
        string_value,
    )
except BaseException:  # noqa: BLE001 -- Import failure still owns any inherited entry descriptor.
    refuse_bootstrap()


def check(deadline: float) -> None:
    """Use the supplied original absolute guest fence, never a reconstructed role clock."""
    now = time.monotonic()
    require(
        math.isfinite(deadline) and math.isfinite(now) and now < deadline and not _observed_signal
    )


def read_exact(peer: socket.socket, count: int, deadline: float) -> bytes:
    """Read a bounded fixed-size frame component under the same original deadline."""
    require(type(count) is int and 0 <= count <= STREAM)
    result = bytearray()
    while len(result) < count:
        check(deadline)
        peer.settimeout(deadline - time.monotonic())
        chunk = peer.recv(min(CHUNK, count - len(result)))
        check(deadline)
        require(bool(chunk))
        result.extend(chunk)
    check(deadline)
    return bytes(result)


def write_all(peer: socket.socket, data: bytes, deadline: float) -> None:
    """Settle all bytes of one bounded socket component without resetting its clock."""
    require(len(data) <= STREAM)
    position = 0
    while position < len(data):
        check(deadline)
        peer.settimeout(deadline - time.monotonic())
        count = peer.send(data[position : position + CHUNK])
        check(deadline)
        require(count > 0)
        position += count
    check(deadline)


def receive_json(peer: socket.socket, deadline: float) -> JsonObject:
    """Decode exactly one four-byte length-prefixed unique-key finite JSON object."""
    count = struct.unpack("!I", read_exact(peer, 4, deadline))[0]
    require(0 < count <= FRAME)
    raw = read_exact(peer, count, deadline)
    require(not raw.startswith(b"\xef\xbb\xbf"))
    value = object_value(decode_json(raw))
    check(deadline)
    return value


def send_json(peer: socket.socket, value: dict[str, object], deadline: float) -> None:
    """Send one complete bounded frame; no newline or client timestamp is authoritative."""
    raw = json.dumps(value, separators=(",", ":"), allow_nan=False).encode()
    require(0 < len(raw) <= FRAME)
    write_all(peer, struct.pack("!I", len(raw)) + raw, deadline)


def matches(argv: list[str]) -> str | None:
    """Gate only the exact native inspect template and a whole lowercase Docker ID."""
    prefix = ["--host", "unix:///var/run/docker.sock", "inspect", "--format", NATIVE_FORMAT]
    if len(argv) == len(prefix) + 1 and argv[:-1] == prefix:
        identifier = argv[-1]
        return identifier if re.fullmatch(r"[a-f0-9]{64}", identifier) else None
    return None


def sha(data: bytes) -> str:
    """Bind private response bytes without printing or interpreting their contents."""
    return hashlib.sha256(data).hexdigest()


def exact(value: JsonObject, names: set[str]) -> JsonObject:
    """Reject missing or unknown fields at each fixed protocol boundary."""
    require(set(value) == names)
    return value


def digest(value: JsonValue) -> str:
    """Require a whole lowercase SHA256 value without coercion."""
    text = string_value(value)
    require(re.fullmatch(r"[a-f0-9]{64}", text) is not None)
    return text


def metadata_stamp(value: JsonValue) -> tuple[int, ...]:
    """Admit the exact original full stat tuple, with booleans excluded."""
    items = array_value(value)
    require(len(items) == STAMP_FIELDS)
    return tuple(integer_value(item) for item in items)


@final
class Original:
    """One fixed fixture leaf; descriptor custody precedes every fallible observation."""

    def __init__(self, path: Path, maximum: int, mode: int, *, empty: bool = False) -> None:
        """Admit a nofollow original, closing it if any subsequent check refuses."""
        self.path = path
        self.fd: int | None = os.open(
            path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC
        )
        try:
            self.before = os.fstat(self.number())
            require(
                stat.S_ISREG(self.before.st_mode)
                and self.before.st_uid == self.before.st_gid == 0
                and stat.S_IMODE(self.before.st_mode) == mode
                and self.before.st_nlink == 1
                and (0 if empty else 1) <= self.before.st_size <= maximum
                and self.path.resolve(strict=True) == self.path
                and ops_guest.stamp(self.path.lstat()) == ops_guest.stamp(self.before)
            )
        except BaseException as error:  # noqa: BLE001 -- Admission must settle its original.
            first = ops_guest.settle((self.close,), error)
            require(first is not None)
            raise cast("BaseException", first) from None

    def number(self) -> int:
        """Borrow only this already-admitted descriptor."""
        require(self.fd is not None)
        return cast("int", self.fd)

    def data(self, deadline: float | None) -> bytes:
        """Read a bounded small original with positional reads and unchanged full stamps."""
        require(self.before.st_size <= FRAME)
        result = bytearray()
        while len(result) < self.before.st_size:
            if deadline is not None:
                check(deadline)
            chunk = os.pread(self.number(), self.before.st_size - len(result), len(result))
            if deadline is not None:
                check(deadline)
            require(bool(chunk))
            result.extend(chunk)
        self.check()
        return bytes(result)

    def hash(self, deadline: float) -> str:
        """Hash the same original in bounded chunks, fencing both sides of every read."""
        hashed = hashlib.sha256()
        offset = 0
        while True:
            check(deadline)
            chunk = os.pread(self.number(), CHUNK, offset)
            check(deadline)
            if not chunk:
                break
            offset += len(chunk)
            require(offset <= self.before.st_size)
            hashed.update(chunk)
        require(offset == self.before.st_size)
        self.check()
        check(deadline)
        return hashed.hexdigest()

    def check(self) -> None:
        """Reject visible namespace or retained metadata replacement, excluding atime."""
        require(
            ops_guest.stamp(os.fstat(self.number())) == ops_guest.stamp(self.before)
            and ops_guest.stamp(self.path.lstat()) == ops_guest.stamp(self.before)
        )

    def close(self) -> None:
        """Attempt this original close once, including a close-error refusal."""
        descriptor, self.fd = self.fd, None
        if descriptor is not None:
            os.close(descriptor)


def private_namespace(original: int, deadline: float) -> None:
    """Require the caller's distinct private mount namespace before any bind effect."""
    check(deadline)
    require(Path("/proc/self/ns/mnt").stat().st_ino != original)
    descriptor = os.open("/proc/self/mountinfo", os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    first: BaseException | None = None
    try:
        data = bytearray()
        while True:
            check(deadline)
            chunk = os.read(descriptor, CHUNK)
            check(deadline)
            if not chunk:
                break
            data.extend(chunk)
            require(len(data) <= 2**20)
        require(bool(data))
        for line in data.splitlines():
            before, separator, _after = line.partition(b" - ")
            fields = before.split()
            require(bool(separator) and len(fields) >= MOUNT_FIELDS)
            require(
                all(
                    not item.startswith((b"shared:", b"master:", b"propagate_from:"))
                    for item in fields[MOUNT_FIELDS:]
                )
            )
    except BaseException as error:  # noqa: BLE001 -- Always close the actual proc descriptor.
        first = error
    first = ops_guest.settle((lambda: os.close(descriptor),), first)
    if first is not None:
        raise first
    check(deadline)


def mount_fixed(  # noqa: C901, PLR0912, PLR0915 -- Two fixed children, attempt-all settlement.
    source: Path, target: Path, deadline: float
) -> None:
    """Run one of the two fixed bind roles in the existing reserved process group."""
    check(deadline)
    require((source, target) in ((DOCKER, ALIAS), (HELPER, DOCKER)))
    require(time.monotonic() + SETTLEMENT < deadline)
    child: subprocess.Popen[bytes] | None = None
    first: BaseException | None = None
    consumed = False
    status: int | None = None
    try:
        child = subprocess.Popen(  # noqa: S603 -- Two literal fixed bind roles, no shell.
            [MOUNT, "--bind", str(source), str(target)],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            cwd=ROOT,
            env=ENV,
            start_new_session=False,
        )
        while True:
            check(deadline - SETTLEMENT)
            status = child.poll()
            if status is not None:
                consumed = True
                break
            time.sleep(min(0.01, deadline - SETTLEMENT - time.monotonic()))
        require(type(status) is int and status == 0)
        check(deadline - SETTLEMENT)
    except BaseException as error:  # noqa: BLE001 -- Retain body failure through child settlement.
        first = error
    if child is not None and not consumed:
        unavailable = False
        try:
            status = child.poll()
            consumed = status is not None
        except BaseException as error:  # noqa: BLE001 -- No signal after uncertain consumption.
            first, unavailable = first or error, True
        if not consumed and not unavailable:
            try:
                child.terminate()
            except BaseException as error:  # noqa: BLE001 -- Still attempt the known wait.
                first = first or error
            try:
                status = child.wait(timeout=max(0.001, deadline - REAP - time.monotonic()))
                consumed = True
            except subprocess.TimeoutExpired:
                pass
            except BaseException as error:  # noqa: BLE001 -- Do not retry an unavailable wait.
                first, unavailable = first or error, True
        if not consumed and not unavailable:
            try:
                child.kill()
            except BaseException as error:  # noqa: BLE001 -- Still consume after a kill error.
                first = first or error
            try:
                status = child.wait(timeout=max(0.001, deadline - time.monotonic()))
                consumed = True
            except BaseException as error:  # noqa: BLE001 -- Original native group is the backstop.
                first = first or error
    if child is not None and not consumed and first is None:
        first = ValueError("inspection_barrier_refused")
    if first is not None:
        raise first
    check(deadline)


@final
class Context:
    """The fixed original configuration, source helper and real Docker capability."""

    def __init__(self) -> None:
        """Retain each original before schema, hash or namespace checks can fail."""
        self.originals: list[Original] = []
        self.inherited: list[int] = []
        try:
            # Only this bounded fixed CONFIG bootstrap precedes knowing its original end.
            self.config = Original(CONFIG, FRAME, PRIVATE)
            self.originals.append(self.config)
            self.raw = self.config.data(None)
            self.value = exact(
                object_value(decode_json(self.raw)),
                {
                    "schemaVersion",
                    "nonce",
                    "bodyEnd",
                    "image",
                    "realCliSha256",
                    "realCliStamp",
                    "namespaceInode",
                    "nativeSha256",
                    "nativeStamp",
                },
            )
            require(type(self.value["schemaVersion"]) is int and self.value["schemaVersion"] == 1)
            self.nonce = string_value(self.value["nonce"])
            require(re.fullmatch(r"[a-f0-9]{32}", self.nonce) is not None)
            end = self.value["bodyEnd"]
            require(type(end) in (int, float))
            self.end = float(cast("int | float", end))
            check(self.end)
            marker, _ = ops_guest.read_fixed(MARKER, 64, PRIVATE)
            require(marker == self.nonce.encode() + b"\n")
            self.image = string_value(self.value["image"])
            require(re.fullmatch(r"sha256:[a-f0-9]{64}", self.image) is not None)
            self.real_sha = digest(self.value["realCliSha256"])
            self.real_stamp = metadata_stamp(self.value["realCliStamp"])
            self.native_sha = digest(self.value["nativeSha256"])
            self.native_stamp = metadata_stamp(self.value["nativeStamp"])
            self.namespace = integer_value(self.value["namespaceInode"])
            require(self.namespace > 0)
            private_namespace(self.namespace, self.end)
            self.helper = self.open(HELPER, 2**20, EXECUTABLE)
            require(ops_guest.stamp(Path(__file__).stat()) == ops_guest.stamp(self.helper.before))
        except BaseException as error:  # noqa: BLE001 -- All admitted sources close on refusal.
            first = ops_guest.settle((self.close,), error)
            require(first is not None)
            raise cast("BaseException", first) from None

    def open(self, path: Path, maximum: int, mode: int, *, empty: bool = False) -> Original:
        """Register an admitted leaf before any subsequent caller observations."""
        check(self.end)
        original = Original(path, maximum, mode, empty=empty)
        self.originals.append(original)
        check(self.end)
        return original

    def check(self) -> None:
        """Retain the original deadline, root marker and source namespaces."""
        check(self.end)
        self.config.check()
        require(self.config.data(self.end) == self.raw)
        self.helper.check()
        require(Path("/proc/self/ns/mnt").stat().st_ino != self.namespace)
        for original in self.originals:
            if original.fd is not None:
                original.check()
        check(self.end)

    def real(self) -> Original:
        """Join the bound alias to the original trusted real Docker stamp and hash."""
        original = self.open(ALIAS, TOOL, EXECUTABLE)
        require(ops_guest.stamp(original.before) == self.real_stamp)
        require(original.hash(self.end) == self.real_sha)
        return original

    def close(self, keep: Original | None = None) -> None:
        """Attempt every known original, preserving only an explicit exec descriptor."""
        actions = [original.close for original in reversed(self.originals) if original is not keep]
        for descriptor in self.inherited[:]:
            self.inherited.remove(descriptor)
            actions.append(lambda descriptor=descriptor: os.close(descriptor))
        first = ops_guest.settle(actions)
        if first is not None:
            raise first


def exec_original(context: Context, original: Original, arguments: list[str]) -> NoReturn:
    """Transfer one checked original to exec; kernel CLOEXEC close status is opaque."""
    context.check()
    original.check()
    descriptor = original.number()
    os.set_inheritable(descriptor, False)  # noqa: FBT003 -- Fixed CLOEXEC transfer.
    context.close(keep=original)
    check(context.end)
    os.execve(  # noqa: S606 -- Validated original FD; only the fixed caller argv is forwarded.
        "/proc/self/fd/" + str(descriptor),
        arguments,
        ENV if original.path == INSTALLED else DOCKER_ENV,
    )


def enter(context: Context, arguments: list[str]) -> NoReturn:
    """Bind exactly the two admitted private namespace leaves, then exec the held Ops FD."""
    require(len(arguments) == ENTER_ARGUMENTS and arguments[0] == "--enter")
    require(re.fullmatch(r"[0-9]+", arguments[1]) is not None)
    descriptor = int(arguments[1])
    revision = arguments[2]
    require(descriptor > STDERR_DESCRIPTOR and re.fullmatch(r"[a-f0-9]{40}", revision) is not None)
    require(all(original.fd != descriptor for original in context.originals))
    if descriptor not in context.inherited:
        context.inherited.append(descriptor)
    require(fcntl.fcntl(descriptor, fcntl.F_GETFL) & os.O_ACCMODE == os.O_RDONLY)
    native = context.open(INSTALLED, BINARY, EXECUTABLE)
    require(ops_guest.stamp(native.before) == context.native_stamp)
    require(ops_guest.stamp(os.fstat(descriptor)) == context.native_stamp)
    require(native.hash(context.end) == context.native_sha)
    # The inherited duplicate is SDK-owned until exec/negative close. Close it
    # explicitly after replacing it with a private same-original retained FD.
    context.inherited.remove(descriptor)
    os.close(descriptor)
    docker = context.open(DOCKER, TOOL, EXECUTABLE)
    require(ops_guest.stamp(docker.before) == context.real_stamp)
    require(docker.hash(context.end) == context.real_sha)
    placeholder = context.open(ALIAS, 0, PRIVATE, empty=True)
    context.check()
    mount_fixed(DOCKER, ALIAS, context.end)
    placeholder.close()
    alias = context.real()
    require(ops_guest.stamp(alias.before) == ops_guest.stamp(docker.before))
    docker.close()
    context.check()
    mount_fixed(HELPER, DOCKER, context.end)
    require(ops_guest.stamp(DOCKER.stat()) == ops_guest.stamp(context.helper.before))
    context.check()
    exec_original(
        context,
        native,
        [
            "simplestchat-ops",
            "release",
            "deploy",
            "--revision",
            revision,
            "--quiet-seconds",
            "0",
            "--timeout-seconds",
            "1800",
            "--finalization-seconds",
            "60",
        ],
    )


def receipt(context: Context, response: JsonObject, identifier: str, out: bytes, err: bytes) -> str:
    """Join original private response bytes to the parent's exact durable stop receipt."""
    raw_sha = digest(response["receiptSha256"])
    original = context.open(RECEIPT, FRAME, PRIVATE)
    raw = original.data(context.end)
    require(sha(raw) == raw_sha)
    value = exact(
        object_value(decode_json(raw)),
        {
            "schemaVersion",
            "scope",
            "nonce",
            "identifier",
            "image",
            "responseRawExit",
            "stdoutBytes",
            "stdoutSha256",
            "stderrBytes",
            "stderrSha256",
            "runningBefore",
            "runningAfter",
            "stopRawExit",
            "finishedChanged",
        },
    )
    require(
        type(value["schemaVersion"]) is int
        and value["schemaVersion"] == 1
        and value["scope"] == "HostDeployInspectionBarrier"
        and value["nonce"] == context.nonce
        and value["identifier"] == identifier
        and value["image"] == context.image
        and type(value["responseRawExit"]) is int
        and value["responseRawExit"] == 0
        and type(value["stopRawExit"]) is int
        and value["stopRawExit"] == 0
        and value["runningBefore"] is True
        and value["runningAfter"] is False
        and value["finishedChanged"] is True
        and integer_value(value["stdoutBytes"]) == len(out)
        and digest(value["stdoutSha256"]) == sha(out)
        and integer_value(value["stderrBytes"]) == len(err)
        and digest(value["stderrSha256"]) == sha(err)
    )
    original.check()
    return raw_sha


def client(context: Context, identifier: str) -> bool:
    """Ask the parent for pass-through or a fully bound original stopped-candidate response."""
    original = context.real()
    context.check()
    metadata = SOCKET.lstat()
    require(stat.S_ISSOCK(metadata.st_mode) and metadata.st_uid == metadata.st_gid == 0)
    require(stat.S_IMODE(metadata.st_mode) == PRIVATE)
    peer = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    first: BaseException | None = None
    deliver = False
    try:
        peer.settimeout(context.end - time.monotonic())
        peer.connect(str(SOCKET))
        context.check()
        require(ops_guest.stamp(SOCKET.lstat()) == ops_guest.stamp(metadata))
        send_json(
            peer,
            {"schemaVersion": 1, "nonce": context.nonce, "identifier": identifier},
            context.end,
        )
        response = receive_json(peer, context.end)
        context.check()
        receipt_sha: str | None = None
        if response.get("action") == "pass":
            _ = exact(response, {"action"})
        else:
            _ = exact(response, {"action", "stdoutBytes", "stderrBytes", "receiptSha256"})
            require(response["action"] == "deliver")
            out_count = integer_value(response["stdoutBytes"])
            err_count = integer_value(response["stderrBytes"])
            require(0 <= out_count <= STREAM and 0 <= err_count <= STREAM)
            out = read_exact(peer, out_count, context.end)
            err = read_exact(peer, err_count, context.end)
            receipt_sha = receipt(context, response, identifier, out, err)
            context.check()
            require(sys.stdout.buffer.write(out) == len(out))
            _ = sys.stdout.buffer.flush()
            check(context.end)
            require(sys.stderr.buffer.write(err) == len(err))
            _ = sys.stderr.buffer.flush()
            check(context.end)
            deliver = True
        confirm_delivery(context, peer, receipt_sha)
        context.check()
        original.check()
    except BaseException as error:  # noqa: BLE001 -- Socket must close after any response refusal.
        first = error
    first = ops_guest.settle((peer.close,), first)
    if first is not None:
        raise first
    context.check()
    return deliver


def confirm_delivery(context: Context, peer: socket.socket, receipt_sha: str | None) -> None:
    """Complete the fixed ACK/checked-parent completion/EOF exchange under the same end."""
    send_json(
        peer,
        {"action": "received", "nonce": context.nonce, "receiptSha256": receipt_sha},
        context.end,
    )
    check(context.end)
    completed = exact(receive_json(peer, context.end), {"action", "nonce", "receiptSha256"})
    require(
        completed
        == {
            "action": "complete",
            "nonce": context.nonce,
            "receiptSha256": receipt_sha,
        }
    )
    check(context.end)
    peer.settimeout(context.end - time.monotonic())
    require(peer.recv(1) == b"")


def interrupted(number: int, _frame: object) -> None:
    """Latch the first observed signal; no later helper success can erase it."""
    global _observed_signal  # noqa: PLW0603 -- One fixed executable lifecycle.
    if not _observed_signal:
        _observed_signal = number


def main(  # noqa: C901 -- One fixed executable lifecycle, all owners and restorations attempted.
    arguments: list[str] | None = None,
) -> int:
    """Keep all failures fixed and settle every admitted owner before returning."""
    args = sys.argv[1:] if arguments is None else arguments
    context: Context | None = None
    first: BaseException | None = None
    handlers = {number: signal.getsignal(number) for number in (signal.SIGINT, signal.SIGTERM)}
    deadline: float | None = None
    passed = False
    inherited: int | None = _entry_descriptors.pop() if _entry_descriptors else None
    try:
        for number in (signal.SIGINT, signal.SIGTERM):
            _ = signal.signal(number, interrupted)
        if args[:1] == ["--enter"]:
            require(len(args) == ENTER_ARGUMENTS and re.fullmatch(r"[0-9]+", args[1]))
            number = int(args[1])
            require(number > STDERR_DESCRIPTOR)
            require(inherited is None or inherited == number)
            inherited = number
            _ = os.fstat(number)
        context = Context()
        if inherited is not None:
            context.inherited.append(inherited)
            inherited = None
        deadline = context.end
        if args[:1] == ["--enter"]:
            enter(context, args)
        identifier = matches(args)
        if identifier is not None and client(context, identifier):
            context.check()
            passed = True
        else:
            exec_original(context, context.real(), [str(DOCKER), *args])
    except BaseException as error:  # noqa: BLE001 -- Never publish dynamic private failure text.
        first = error
    actions: list[Callable[[], None]] = []
    if inherited is not None:
        actions.append(lambda: os.close(inherited))
    if context is not None:
        actions.append(context.close)

    def restore_handlers() -> None:
        first_restore: BaseException | None = None
        for number, handler in handlers.items():
            try:
                _ = signal.signal(number, handler)
            except BaseException as error:  # noqa: BLE001 -- Attempt both original restorations.
                first_restore = first_restore or error
        if first_restore is not None:
            raise first_restore

    actions.append(restore_handlers)
    first = ops_guest.settle(actions, first)
    return status_after_close(passed=passed, first=first, deadline=deadline)


def status_after_close(
    *,
    passed: bool,
    first: BaseException | None,
    deadline: float | None,
) -> int:
    """Run the final original signal/clock fence only after explicit owners and restores."""
    try:
        require(deadline is not None)
        check(cast("float", deadline))
    except BaseException as error:  # noqa: BLE001 -- Original post-restoration fence is final.
        first = first or error
    if passed and first is None:
        return 0
    try:
        require(sys.stderr.buffer.write(FAILURE) == len(FAILURE))
        _ = sys.stderr.buffer.flush()
    except (OSError, ValueError):
        return 1
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
