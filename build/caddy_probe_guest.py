"""Finite, disposable-guest Caddy experiment; never a production maintenance route.

Only the controller's freshly seeded root fixture may execute this file. The
outer KVM clock remains authoritative. No private CA/key material leaves this
VM; stdout is one closed metadata record after all known owners settle.
"""

from __future__ import annotations

import argparse
import errno
import hashlib
import http.client
import io
import ipaddress
import json
import math
import os
import re
import shutil
import signal
import socket
import ssl
import stat
import subprocess
import sys
import tarfile
import time
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path
from typing import TYPE_CHECKING, BinaryIO, Literal, NoReturn, TypedDict, cast

# -I deliberately omits the script directory. Admit only this fixed root-owned
# staging directory before importing the two explicitly transferred helpers.
_STAGE = Path(__file__).parent
if not (
    _STAGE.is_absolute()
    and _STAGE.resolve() == _STAGE
    and _STAGE.stat().st_uid == os.getuid()
    and _STAGE.stat().st_mode & 0o022 == 0
):
    _failure = "prototype_stage_refused"
    raise SystemExit(_failure)
sys.path.insert(0, str(_STAGE))
import bounded_process  # noqa: E402 -- Admit the fixed root-owned -I staging path first.
import caddy_materials  # noqa: E402 -- Same fixed owned staging path; no ambient imports.

if TYPE_CHECKING:
    from collections.abc import Callable, Sequence

type JsonValue = caddy_materials.JsonValue


class Report(TypedDict):
    """Closed metadata-only guest projection; no private bytes or path diagnostics."""

    schemaVersion: int
    scope: str
    passed: bool
    cleanupPassed: bool
    failure: str | None
    image: str
    sourceClosure: str
    materials: dict[str, JsonValue]
    observations: list[dict[str, JsonValue]]
    containment: dict[str, JsonValue]
    qualification: str
    checkpoint: str
    exception: str | None
    cleanupException: str | None
    ioCheckpoint: str | None
    errnoCategory: str | None
    tlsCheckpoint: str | None
    tlsFailureCategory: str | None


PHASES = frozenset(
    (
        "admission",
        "guest-prerequisites",
        "source-setup",
        "image",
        "network",
        "containment",
        "module-graph",
        "natural-tls",
        "trust-install",
        "fresh-process",
        "negative-tls",
        "validator",
        "negative-copies",
        "final-observations",
        "body-completion",
        "cleanup",
        "restore",
        "final-fence",
        "complete",
    )
)


IO_CHECKPOINTS = frozenset(
    {
        "containment-route",
        "containment-rule-file",
        "containment-guard-install",
        "containment-capture-start",
        "containment-counter-before",
        "containment-marker-open",
        "containment-marker-send",
        "containment-marker-close",
        "containment-counter-after",
        "containment-counter-validate",
        "containment-capture-check",
    }
)
ERRNO_CATEGORIES = {
    errno.EPERM: "EPERM",
    errno.EACCES: "EACCES",
    errno.ENOENT: "ENOENT",
    errno.EBADF: "EBADF",
    errno.ENOSPC: "ENOSPC",
    errno.EIO: "EIO",
    errno.EINTR: "EINTR",
    errno.EPIPE: "EPIPE",
    errno.EAGAIN: "EAGAIN",
}
MARKER = b"INERT-CONTAINMENT-PROBE"
MAX_COUNTER = 2**64 - 1


def errno_category(error: BaseException | None) -> str | None:
    """Expose only fixed symbolic errno categories, never strerror, filenames or arguments."""
    if not isinstance(error, OSError):
        return None
    if error.errno is None:
        return "UNKNOWN"
    return ERRNO_CATEGORIES.get(error.errno, "OTHER")


def io_checkpoint(report: Report | None, checkpoint: str) -> None:
    """Record only this fixed containment operation's local observation boundary."""
    if report is not None:
        report["ioCheckpoint"] = checkpoint


def exception_kind(error: BaseException | None) -> str | None:
    """Classify a fixed family without exposing any error text, class name or arguments."""
    if error is None:
        return None
    catalog = (
        (TimeoutError, "timeout"),
        (KeyboardInterrupt, "signal"),
        (PrototypeError, "policy"),
        ((ValueError, UnicodeError), "decode"),
        (OSError, "io"),
    )
    for family, kind in catalog:
        if isinstance(error, family):
            return kind
    return "unexpected"


@dataclass
class Effects:
    """Record effects before fallible command evidence finalization."""

    guarded: bool = False
    network_id: str | None = None


class Options(argparse.Namespace):
    """Explicit experimental clock options; no production operation selectors."""

    run_id: str = ""
    seconds: float = 0
    finalization_seconds: float = 0


IMAGE = (
    "ghcr.io/swiftraccoon/simplestchat-caddy:"
    "2.11.7-73c81fdbfe625d3df3989ba807064c81fbe2c071"
    "@sha256:23c127cb8945a1de0c6753010614b50af1643bbcf93ab8a9a7d7461efd50b7c0"
)
CLOSURE = "20e5d12552abd99ef774a3b30f597ea38d9f39ff64ce11f8c4975298d9e29205"
CADDYFILE = "cb48cb809b5f80cb4187f3976be868c1d71a8411ad893ba0ad2b4df03ffd6039"
DOMAIN = "maintain.localhost"
QUALIFICATION = "TLS-only; no application or maintenance success claim"
MATERIALS = (
    "pki/authorities/local/root.crt",
    "pki/authorities/local/root.key",
    "pki/authorities/local/intermediate.crt",
    "pki/authorities/local/intermediate.key",
    "certificates/local/maintain.localhost/maintain.localhost.crt",
    "certificates/local/maintain.localhost/maintain.localhost.key",
    "certificates/local/maintain.localhost/maintain.localhost.json",
)
MAX_CAPTURE = 2 * 1024**2
MAX_COMMANDS = 128
APP_UID = 10001
PRIVATE_FILE = 0o600
PRIVATE_DIR = 0o700
CHUNK = 65536
MAX_CHAIN = 3
TLS_ONLY_STATUS = 404
PIDS = 128
PCAP_HEADER = 24
MAX_METADATA = 131072
MAX_CONTAINER_EXIT = 255
MAX_WIRE_ERRORS = 8192
FIXTURE_NANO_CPUS = 500_000_000
NORMAL_STOP_SECONDS = 30
CONTAINER_TMP = "/tmp"  # noqa: S108 -- Exact reviewed container tmpfs destination.
TLS_CHECKPOINTS = frozenset({"connect", "handshake", "chain", "request", "response", "close"})
TLS_FAILURE_CATEGORIES = frozenset(
    {
        "timeout",
        "connection-refused",
        "connection-reset",
        "network-unreachable",
        "host-unreachable",
        "address-unavailable",
        "permission",
        "certificate-verification",
        "tls",
        "io",
        "policy",
        "unexpected",
    }
)


class PrototypeError(Exception):
    """Carry only a fixed nonsecret failure code."""


def refuse(code: str) -> NoReturn:
    """Raise only an already-fixed safe failure code."""
    raise PrototypeError(code)


def require(value: object, code: str) -> None:
    """Refuse an unproved positive boundary."""
    if not value:
        raise PrototypeError(code)


def settle(
    actions: Sequence[Callable[[], object]],
    first: BaseException | None = None,
) -> BaseException | None:
    """Attempt every registered peer without replacing the earliest exception."""
    for action in actions:
        try:
            _ = action()
        except BaseException as error:  # noqa: BLE001 -- Settle every known peer.
            first = first or error
    return first


def obj(value: object) -> dict[str, object]:
    """Require an ordinary string-key JSON object."""
    require(type(value) is dict, "metadata_object")
    result = cast("dict[object, object]", value)
    require(all(type(key) is str for key in result), "metadata_object")
    return cast("dict[str, object]", result)


def array(value: object) -> list[object]:
    """Require an ordinary JSON array."""
    require(type(value) is list, "metadata_array")
    return cast("list[object]", value)


def text(value: object) -> str:
    """Require an ordinary metadata string, never coerce bytes or objects."""
    require(type(value) is str, "metadata_string")
    return cast("str", value)


def decode(data: bytes) -> object:
    """Parse bounded tool JSON; malformed values never reach a positive boundary."""
    require(len(data) <= MAX_CAPTURE, "metadata_bound")
    return cast("object", json.loads(data))


@dataclass
class Control:
    """Subordinate guest duration; the controller owns the original whole clock."""

    started: float
    seconds: float
    finalization: float
    cancelled: bool = False
    settling: bool = False

    @property
    def end(self) -> float:
        """Do not reset this guest handoff clock."""
        return self.started + self.seconds

    def check(self) -> None:
        """Poll the live duration and original signal through finalization."""
        now = time.monotonic()
        ceiling = self.end if self.settling else self.end - self.finalization
        require(math.isfinite(now) and self.started <= now < ceiling, "guest_deadline")
        require(not self.cancelled or self.settling, "guest_cancelled")

    def interrupt(self, _number: int, _frame: object) -> None:
        """Latch without throwing through a newly admitted owner."""
        self.cancelled = True

    def remaining(self, maximum: float) -> float:
        """Clip each fixed role to the current live ceiling."""
        self.check()
        ceiling = self.end if self.settling else self.end - self.finalization
        return min(maximum, ceiling - time.monotonic())


def private_file(path: Path) -> BinaryIO:
    """Admit an exclusive original before changing its final fixture mode."""
    fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    return os.fdopen(fd, "w+b")


def write_file(path: Path, data: bytes, mode: int = 0o600) -> None:
    """Publish complete fixture bytes before a deliberately selected final mode."""
    stream = private_file(path)
    first = None
    try:
        _ = stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())
        os.fchmod(stream.fileno(), mode)
    except BaseException as error:  # noqa: BLE001 -- Settle every known peer.
        first = error
    first = settle((stream.close,), first)
    if first is not None:
        raise first


def identity_stamp(value: os.stat_result) -> tuple[int, ...]:
    """Compare read-relevant identity/content metadata while permitting access-time updates."""
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


def bounded_original(path: Path, maximum: int, control: Control) -> bytes:
    """Read one admitted regular original within a fixed finite experimental byte profile."""
    control.check()
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    first, data = None, b""
    try:
        stamp = os.fstat(fd)
        require(
            stat.S_ISREG(stamp.st_mode) and stamp.st_nlink == 1 and 0 < stamp.st_size <= maximum,
            "input_profile",
        )
        data = os.pread(fd, maximum + 1, 0)
        require(
            len(data) == stamp.st_size
            and identity_stamp(os.fstat(fd)) == identity_stamp(stamp)
            and identity_stamp(path.lstat()) == identity_stamp(stamp),
            "input_changed",
        )
        control.check()
    except BaseException as error:  # noqa: BLE001 -- Keep observed cause through checked close.
        first = error
    first = settle((lambda: os.close(fd),), first)
    if first is not None:
        raise first
    return data


@dataclass
class Tool:
    """Retain the actual absolute binary used, without attributing ambient PATH aliases."""

    path: Path
    fd: int
    stamp: os.stat_result
    sha256: str

    @classmethod
    def open(cls, name: str, control: Control) -> Tool:
        """Admit a root-owned packaged executable after fixed system-only resolution."""
        control.check()
        resolved = shutil.which(name, path="/usr/sbin:/usr/bin:/sbin:/bin")
        require(resolved is not None, "tool_missing")
        path = Path(cast("str", resolved)).resolve(strict=True)
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
        try:
            stamp = os.fstat(fd)
            require(
                stat.S_ISREG(stamp.st_mode)
                and stamp.st_uid == 0
                and not stamp.st_mode & 0o022
                and 0 < stamp.st_size <= 256 * 1024**2,
                "tool_profile",
            )
            tool = cls(path, fd, stamp, "")
            tool.sha256 = tool.digest(control)
            tool.check(control)
        except BaseException as error:  # noqa: BLE001 -- Close an admitted original.
            raise cast("BaseException", settle((lambda: os.close(fd),), error)) from None
        else:
            return tool

    def digest(self, control: Control) -> str:
        """Bound each position-independent original read; never share a child cursor."""
        digest = hashlib.sha256()
        offset = 0
        while offset < self.stamp.st_size:
            control.check()
            chunk = os.pread(self.fd, min(CHUNK, self.stamp.st_size - offset), offset)
            require(chunk, "tool_short_read")
            digest.update(chunk)
            offset += len(chunk)
            control.check()
        return digest.hexdigest()

    def check(self, control: Control) -> None:
        """Refuse observed namespace or full original stamp changes."""
        control.check()

        def fields(value: os.stat_result) -> tuple[int, ...]:
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

        require(
            fields(os.fstat(self.fd)) == fields(self.stamp) == fields(self.path.lstat()),
            "tool_changed",
        )
        control.check()

    def finish(self, control: Control) -> None:
        """Rehash and close once even when final validation refuses."""
        first = None
        try:
            self.check(control)
            require(self.digest(control) == self.sha256, "tool_changed")
        except BaseException as error:  # noqa: BLE001 -- Actual close still runs.
            first = error
        first = settle((lambda: os.close(self.fd),), first)
        if first is not None:
            raise first


@dataclass
class EffectOutput:
    """Keep observed effect bytes before buffered evidence flush/close can fail."""

    file: BinaryIO
    observed: bytearray = field(default_factory=bytearray)

    def write(self, data: bytes) -> int:
        """Mirror only the existing bounded stdout supplied by the mature pump."""
        self.observed.extend(data)
        written = self.file.write(data)
        require(written == len(data), "evidence_short_write")
        return written


@dataclass
class Commands:
    """Use the unchanged bounded POSIX pump with checked original evidence closes."""

    root: Path
    control: Control
    count: int = 0
    owned: list[str] = field(default_factory=list)
    create_uncertain: bool = False
    tools: dict[str, str] = field(default_factory=dict)
    tool_owners: dict[str, Tool] = field(default_factory=dict)
    watch: Watch | None = None

    def call(  # noqa: PLR0913, PLR0917 -- Independent fixed role limits and effect admission.
        self,
        role: str,
        argv: Sequence[str],
        seconds: float = 30,
        accepted: tuple[int, ...] = (0,),
        adopt: Callable[[bytes], None] | None = None,
        maximum: int = MAX_CAPTURE,
    ) -> bytes:
        """Adopt raw-zero/natural-EOF effects before fallible evidence finalization."""
        self.control.check()
        require(self.count < MAX_COMMANDS, "command_count")
        require(re.fullmatch(r"[a-z0-9-]{1,48}", role), "command_role")
        program = self.tools.get(argv[0])
        require(program is not None, "unowned_tool")
        if argv[0] in self.tool_owners:
            self.tool_owners[argv[0]].check(self.control)
        prefix = self.root / f"{self.count:03d}-{role}"
        self.count += 1
        streams: list[BinaryIO] = []
        data, status, first = b"", None, None
        try:
            # Register the first admitted stream even if opening the second fails.
            for suffix in (".out", ".err"):
                streams.append(  # noqa: PERF401 -- Keep each admitted peer.
                    private_file(prefix.with_suffix(suffix))
                )
            effect_output = EffectOutput(streams[0]) if adopt is not None else None
            status, _, _ = bounded_process.run(
                [cast("str", program), *argv[1:]],
                env={"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LC_ALL": "C"},
                output=(
                    cast("BinaryIO", cast("object", effect_output))
                    if effect_output is not None
                    else streams[0]
                ),
                error=streams[1],
                limits=bounded_process.Limits(
                    timeout=self.control.remaining(seconds),
                    stdout=maximum,
                    stderr=MAX_CAPTURE,
                    healthy=self.healthy,
                    grace=min(5, self.control.remaining(5)),
                ),
            )
            if status == 0 and adopt is not None and effect_output is not None:
                adopt(bytes(effect_output.observed))
            streams[0].flush()
            _ = streams[0].seek(0)
            data = streams[0].read(maximum + 1)
            require(len(data) <= maximum, "command_capture")
            self.control.check()
            if argv[0] in self.tool_owners:
                self.tool_owners[argv[0]].check(self.control)
        except BaseException as error:  # noqa: BLE001 -- Settle every known peer.
            first = error
        first = settle(tuple(stream.close for stream in streams), first)
        if first is not None:
            raise first
        require(status in accepted, "command_status")
        return data

    def healthy(self) -> bool:
        """Give quiet process pumps the same live clock/cancellation predicate."""
        try:
            self.control.check()
            if self.watch is not None and not self.watch.closed:
                self.watch.check()
        except PrototypeError:
            return False
        return True

    def create(self, role: str, arguments: Sequence[str]) -> str:
        """Require complete raw-zero ID output before authorizing any cleanup."""
        captured: list[str] = []

        def adopt(data: bytes) -> None:
            require(re.fullmatch(b"[a-f0-9]{64}\n", data), "container_id")
            identifier = data.strip().decode("ascii")
            self.owned.append(identifier)
            self.create_uncertain = False
            captured.append(identifier)

        self.create_uncertain = True
        _ = self.call(role, ["docker", "create", *arguments], adopt=adopt)
        require(len(captured) == 1, "container_unknown")
        return captured[0]

    def remove(self, identifier: str) -> None:
        """Remove only this experiment's complete known immutable container ID."""
        require(identifier in self.owned, "foreign_container")

        def removed(_data: bytes) -> None:
            self.owned.remove(identifier)

        _ = self.call("remove", ["docker", "rm", "--force", identifier], adopt=removed)


@dataclass
class Original:
    """Retain one original material plus its original anchored parent directory."""

    parent: int
    fd: int
    path: Path
    stamp: os.stat_result
    parent_stamp: os.stat_result
    data: bytes

    @classmethod
    def open(cls, path: Path, control: Control) -> Original:
        """No symlink or reopened material supplies the accepted bytes."""
        control.check()
        require(path.resolve(strict=True) == path, "material_spelling")
        parent = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        fd = None
        try:
            fd = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=parent)
            observed = os.fstat(fd)
            require(
                stat.S_ISREG(observed.st_mode)
                and stat.S_IMODE(observed.st_mode) == PRIVATE_FILE
                and observed.st_uid == APP_UID
                and observed.st_gid == APP_UID
                and observed.st_nlink == 1
                and 0 < observed.st_size <= CHUNK,
                "material_profile",
            )
            parent_stamp = os.fstat(parent)
            require(
                parent_stamp.st_uid == APP_UID
                and parent_stamp.st_gid == APP_UID
                and stat.S_IMODE(parent_stamp.st_mode) == PRIVATE_DIR,
                "material_parent_profile",
            )
            data = os.pread(fd, 65537, 0)
            require(len(data) == observed.st_size, "material_short_read")
            result = cls(parent, fd, path, observed, parent_stamp, data)
            result.check(control)
        except BaseException as error:  # noqa: BLE001 -- Settle every known peer.
            failure = settle(
                tuple(
                    lambda item=item: os.close(item) for item in (fd, parent) if item is not None
                ),
                error,
            )
            raise cast("BaseException", failure) from None
        else:
            return result

    def check(self, control: Control) -> None:
        """Seven exact bytes/stamps survive; parent timestamps may change for auxiliary writes."""
        control.check()
        require(self.path.resolve(strict=True) == self.path, "material_spelling")
        control.check()

        def fields(value: os.stat_result) -> tuple[int, ...]:
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

        def directory(value: os.stat_result) -> tuple[int, ...]:
            return (
                value.st_dev,
                value.st_ino,
                value.st_mode,
                value.st_uid,
                value.st_gid,
            )

        require(
            fields(os.fstat(self.fd))
            == fields(self.stamp)
            == fields(os.stat(self.path.name, dir_fd=self.parent, follow_symlinks=False))
            and directory(os.fstat(self.parent))
            == directory(self.parent_stamp)
            == directory(self.path.parent.lstat())
            and os.pread(self.fd, 65537, 0) == self.data,
            "material_changed",
        )
        control.check()

    def close(self) -> None:
        """Attempt both actual checked closes even after the first fails."""
        first = settle((lambda: os.close(self.fd), lambda: os.close(self.parent)))
        if first is not None:
            raise first


@dataclass
class TrustCopy:
    """Retain the exact new guest-only public-root file until checked anchored retirement."""

    file: BinaryIO
    view: Original

    @classmethod
    def create(cls, path: Path, data: bytes, control: Control) -> TrustCopy:
        """Use exclusive creation; failed admission never authorizes a foreign unlink."""
        control.check()
        parent = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        stream = None
        first = None
        result = None
        try:
            stream = private_file(path)
            _ = stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
            os.fchmod(stream.fileno(), 0o644)
            view = Original(
                parent,
                stream.fileno(),
                path,
                os.fstat(stream.fileno()),
                os.fstat(parent),
                data,
            )
            view.check(control)
            result = cls(stream, view)
        except BaseException as error:  # noqa: BLE001 -- Retire only an admitted original.
            first = error
        if first is not None:
            actions: list[Callable[[], object]] = []
            if stream is not None:
                admitted = stream

                def remove_admitted() -> None:
                    original = os.fstat(admitted.fileno())
                    named = os.stat(path.name, dir_fd=parent, follow_symlinks=False)
                    require(
                        (original.st_dev, original.st_ino) == (named.st_dev, named.st_ino),
                        "guest_root_copy_changed",
                    )
                    os.unlink(path.name, dir_fd=parent)

                actions.extend((remove_admitted, admitted.close))
            actions.append(lambda: os.close(parent))
            raise cast("BaseException", settle(actions, first)) from None
        require(result is not None, "guest_root_copy")
        return cast("TrustCopy", result)

    def finish(self, control: Control) -> None:
        """Check the retained original before unlink; always close both actual originals."""
        first = None
        try:
            self.view.check(control)
            os.unlink(self.view.path.name, dir_fd=self.view.parent)
        except BaseException as error:  # noqa: BLE001 -- Never replace observed substitution.
            first = error
        first = settle((self.file.close, lambda: os.close(self.view.parent)), first)
        if first is not None:
            raise first


def firewall(table: str, interface: str, bridge: str, peer: str) -> str:
    """Confine dockerd's host DNS as well as container forwarding; no generic established bypass."""
    require(re.fullmatch(r"sc_caddy_[a-f0-9]{12}", table), "firewall_identity")
    for name in (interface, bridge):
        require(re.fullmatch(r"[a-zA-Z0-9_-]{1,15}", name), "interface_identity")
    require(peer == "10.0.2.2", "control_peer")
    control_rule = f'oifname "{interface}" ip daddr {peer} tcp sport 22 ct state established'
    return f"""table inet {table} {{
 counter blocked_output {{ }}
 counter blocked_forward {{ }}
 counter ssh_control {{ }}
 chain output {{ type filter hook output priority -1; policy drop;
  oifname "lo" accept
  oifname "{bridge}" accept
  {control_rule} counter name ssh_control accept
  counter name blocked_output drop
 }}
 chain forward {{ type filter hook forward priority -1; policy drop;
  iifname "{bridge}" oifname "{bridge}" accept
  counter name blocked_forward drop
 }}
}}
table netdev {table} {{
 counter blocked_link {{ }}
 chain egress {{ type filter hook egress device "{interface}" priority 0; policy drop;
  ip daddr {peer} tcp sport 22 accept
  arp daddr ip {peer} accept
  counter name blocked_link drop
 }}
}}\n"""


@dataclass
class Watch:
    """Observe every non-control physical IP frame while the exhaustive guest guard is live."""

    child: subprocess.Popen[bytes]
    output: BinaryIO
    error: BinaryIO
    commands: Commands
    closed: bool = False

    @classmethod
    def start(cls, commands: Commands, interface: str, peer: str) -> Watch:
        """Keep the exact capture child reserved until the bounded checked finalizer."""
        if "tcpdump" in commands.tool_owners:
            commands.tool_owners["tcpdump"].check(commands.control)
        output = private_file(commands.root / "wire.pcap")
        error = None
        child = None
        try:
            error = private_file(commands.root / "wire.err")
            argv = [
                commands.tools["tcpdump"],
                "-p",
                "-n",
                "-U",
                "-i",
                interface,
                "-s",
                "128",
                "-c",
                "4096",
                "-w",
                "-",
                f"not (ip and host {peer} and tcp port 22) and not (arp and host {peer})",
            ]
            child = subprocess.Popen(  # noqa: S603 -- Fixed retained tcpdump and generated filter.
                argv,
                stdin=subprocess.DEVNULL,
                stdout=output,
                stderr=error,
                start_new_session=True,
                env={"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LC_ALL": "C"},
            )
            result = cls(child, output, error, commands)
            end = time.monotonic() + commands.control.remaining(5)
            while time.monotonic() < end:
                commands.control.check()
                require(not bounded_process.leader_exited(child.pid), "wire_capture_stopped")
                require(os.fstat(error.fileno()).st_size <= MAX_WIRE_ERRORS, "wire_error_bound")
                if b"listening on" in os.pread(error.fileno(), MAX_WIRE_ERRORS, 0):
                    return result
                time.sleep(0.02)
            refuse("wire_capture_not_ready")
        except BaseException as first:  # noqa: BLE001 -- Register and settle admitted peers.
            actions: list[Callable[[], object]] = []
            if child is not None and child.returncode is None:
                actions.append(lambda: bounded_process.stop(child, grace=1))
            actions.extend(stream.close for stream in (output, error) if stream is not None)
            raise cast("BaseException", settle(actions, first)) from None

    def check(self) -> None:
        """Refuse a full or exited capture before claiming contact absence."""
        self.commands.control.check()
        require(not bounded_process.leader_exited(self.child.pid), "wire_capture_stopped")
        require(os.fstat(self.output.fileno()).st_size <= 1024**2, "wire_capture_bound")
        require(os.fstat(self.error.fileno()).st_size <= MAX_WIRE_ERRORS, "wire_error_bound")

    def finish(self) -> None:
        """Stop unreaped exact process; require pcap has no noncontrol physical packets."""
        if self.closed:
            return
        self.closed = True
        first = None
        try:
            self.check()
            os.killpg(self.child.pid, signal.SIGINT)
            _ = bounded_process.wait_exited(self.child.pid, self.commands.control.remaining(5))
            bounded_process.stop(self.child, grace=1)
            require(self.child.returncode == 0, "wire_capture_exit")
            self.output.flush()
            require(
                os.fstat(self.output.fileno()).st_size == PCAP_HEADER,
                "external_contact_observed",
            )
        except BaseException as error:  # noqa: BLE001 -- Settle every known peer.
            first = error
        actions: list[Callable[[], object]] = []
        if self.child.returncode is None:
            actions.append(lambda: bounded_process.stop(self.child, grace=1))
        actions.extend((self.output.close, self.error.close))
        first = settle(actions, first)
        if first is not None:
            raise first


def check_guard_retained(commands: Commands, family: str, table: str) -> None:
    """Observe confinement; the exact owned VM exit retires guest-only kernel resources."""
    require(
        not commands.owned and not commands.create_uncertain,
        "guard_retained_for_live_or_unknown_container",
    )
    observed = commands.call(
        "guard-retained-" + family, ["nft", "-json", "list", "table", family, table]
    )
    require(b'"drop"' in observed, "guard_retention_unproved")


def normal_arguments(root: Path, data: Path, config: Path, bridge: str) -> list[str]:
    """Preserve the normal reviewed config/mounts/UID, without injecting TLS configuration."""
    return [
        "--pull",
        "never",
        "--network",
        bridge,
        "--user",
        "10001:10001",
        "--init",
        "--restart",
        "unless-stopped",
        "--stop-timeout",
        "30",
        "--read-only",
        "--cap-drop",
        "ALL",
        "--cap-add",
        "NET_BIND_SERVICE",
        "--security-opt",
        "no-new-privileges",
        "--memory",
        "256m",
        "--cpus",
        "0.5",
        "--pids-limit",
        "128",
        "--sysctl",
        "net.ipv4.ip_unprivileged_port_start=0",
        "--log-driver",
        "local",
        "--log-opt",
        "max-size=10m",
        "--log-opt",
        "max-file=3",
        "--env-file",
        str(root / "proxy.env"),
        "--mount",
        f"type=bind,src={root / 'Caddyfile'},dst=/etc/caddy/Caddyfile,readonly",
        "--mount",
        f"type=bind,src={data},dst=/data",
        "--mount",
        f"type=bind,src={config},dst=/config",
        "--mount",
        (f"type=bind,src={root / 'browser-assets'},dst=/run/simplestchat-browser-assets,readonly"),
        "--tmpfs",
        "/tmp:rw,nosuid,nodev,noexec,size=32m,mode=1777",  # noqa: S108 -- Reviewed container tmpfs.
        "--publish",
        "127.0.0.1:443:443/tcp",
        "--publish",
        "127.0.0.1:443:443/udp",
        "--publish",
        "127.0.0.1:80:80/tcp",
        IMAGE,
    ]


def validate(commands: Commands) -> dict[str, JsonValue]:
    """Execute the original launcher network-none8MiB validator; no TLS overrides."""
    identifier = commands.create(
        "validator-create",
        [
            "--name",
            "validation-" + commands.root.name,
            "--cidfile",
            str(commands.root / "validator.cid"),
            "--label",
            "simplestchat.public.validation=" + commands.root.name,
            "--network",
            "none",
            "--pull",
            "never",
            "--read-only",
            "--user",
            "10001:10001",
            "--cap-drop",
            "ALL",
            "--cap-add",
            "NET_BIND_SERVICE",
            "--security-opt",
            "no-new-privileges",
            "--memory",
            "256m",
            "--pids-limit",
            "128",
            "--log-driver",
            "local",
            "--log-opt",
            "max-size=10m",
            "--log-opt",
            "max-file=3",
            "--env-file",
            str(commands.root / "proxy.env"),
            "--mount",
            (f"type=bind,src={commands.root / 'Caddyfile'},dst=/etc/caddy/Caddyfile,readonly"),
            "--tmpfs",
            "/data:rw,uid=10001,gid=10001,size=8m",
            "--tmpfs",
            "/config:rw,uid=10001,gid=10001,size=8m",
            IMAGE,
            "caddy",
            "validate",
            "--config",
            "/etc/caddy/Caddyfile",
        ],
    )
    _ = commands.call("validator-start", ["docker", "start", identifier])
    value = commands.call("validator-wait", ["docker", "wait", identifier])
    require(value == b"0\n", "validator_failed")
    _ = commands.call("validator-logs", ["docker", "logs", identifier])
    commands.remove(identifier)
    return {"phase": "original-network-none-8m-validator", "exitStatus": 0}


@dataclass(frozen=True)
class Bridge:
    """Join only the actual created network and the already inspected pinned image."""

    identifier: str
    name: str
    image: str


@dataclass(frozen=True)
class Endpoint:
    """Keep a private numeric target joined to this sole owner's immutable container ID."""

    container: str
    bridge: Bridge
    address: str
    prefix: int
    endpoint: str
    subnet: str
    gateway: str
    operational_bindings: int

    def check(self, commands: Commands) -> None:
        """Refuse observed identity/address replacement before another separate TLS probe."""
        require(owned_endpoint(commands, self.container, self.bridge) == self, "endpoint_changed")
        commands.control.check()

    def facts(self) -> dict[str, JsonValue]:
        """Never expose the private address, gateway, subnet or raw daemon response."""
        return {
            "transport": "owned-internal-bridge-ipv4",
            "operationalLoopbackBindingCount": self.operational_bindings,
            "publishedLoopbackPathProved": False,
        }


def one_inspection(raw: bytes) -> dict[str, object]:
    """Admit one bounded daemon object; all queries use already owned IDs."""
    rows = array(decode(raw))
    require(len(rows) == 1, "endpoint_inspection")
    return obj(rows[0])


def operational_bindings(value: object) -> int:
    """Distinguish requested ports from absent or actual fixed loopback publication."""
    if value is None:
        return 0
    ports = obj(value)
    expected = {"80/tcp": "80", "443/tcp": "443", "443/udp": "443"}
    require(set(ports).issubset({*expected, "2019/tcp"}), "endpoint_operational_ports")
    count = 0
    for name, bindings in ports.items():
        if name == "2019/tcp":
            require(bindings is None or bindings == [], "endpoint_operational_ports")
            continue
        require(
            bindings is None
            or bindings == []
            or bindings == [{"HostIp": "127.0.0.1", "HostPort": expected[name]}],
            "endpoint_operational_ports",
        )
        if bindings:
            count += 1
    return count


def bridge_addresses(
    member: dict[str, object], network: dict[str, object]
) -> tuple[str, int, str, str]:
    """Use mature IPv4 parsing; refuse noncanonical, non-RFC1918 and gateway targets."""
    configurations = array(obj(network["IPAM"])["Config"])
    require(len(configurations) == 1, "endpoint_ipam")
    configuration = obj(configurations[0])
    subnet_text = text(configuration["Subnet"])
    gateway_text = text(configuration["Gateway"])
    address_text = text(member["IPAddress"])
    subnet = ipaddress.IPv4Network(subnet_text, strict=True)
    gateway = ipaddress.IPv4Address(gateway_text)
    address = ipaddress.IPv4Address(address_text)
    prefix = member["IPPrefixLen"]
    private = tuple(
        ipaddress.IPv4Network(value) for value in ("10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16")
    )
    require(
        str(subnet) == subnet_text
        and str(gateway) == gateway_text
        and str(address) == address_text
        and type(prefix) is int
        and prefix == subnet.prefixlen
        and any(subnet.subnet_of(value) for value in private)
        and address in subnet
        and gateway in subnet
        and address not in (subnet.network_address, subnet.broadcast_address, gateway)
        and not address.is_loopback
        and not address.is_link_local
        and not address.is_multicast
        and not address.is_unspecified
        and address != ipaddress.IPv4Address("10.0.2.2"),
        "endpoint_ipv4",
    )
    member_gateway = member.get("Gateway")
    require(
        type(member_gateway) is str and member_gateway in ("", gateway_text),
        "endpoint_gateway",
    )
    return address_text, cast("int", prefix), subnet_text, gateway_text


def container_identity(identifier: str, bridge: Bridge, row: dict[str, object]) -> None:
    """Bind a daemon observation to the exact created ID and previously inspected image."""
    require(
        re.fullmatch(r"[a-f0-9]{64}", identifier)
        and re.fullmatch(r"[a-f0-9]{64}", bridge.identifier)
        and re.fullmatch(r"sha256:[a-f0-9]{64}", bridge.image)
        and row["Id"] == identifier
        and row["Image"] == bridge.image
        and obj(row["Config"])["Image"] == IMAGE,
        "endpoint_container",
    )


def bridge_identity(bridge: Bridge, network: dict[str, object]) -> None:
    """Bind the original created network ID rather than only a network name."""
    require(
        network["Id"] == bridge.identifier
        and network["Name"] == bridge.name
        and network["Driver"] == "bridge"
        and network["Scope"] == "local"
        and network["Internal"] is True
        and network["EnableIPv6"] is False
        and obj(network["Options"])["com.docker.network.bridge.name"] == bridge.name,
        "endpoint_network",
    )


@dataclass(frozen=True)
class Refusal:
    """Carry an observed nonzero exited/backoff state, never terminal or live-TLS authority."""

    state: Literal["exited", "restarting"]
    exit_status: int


def observed_refusal(row: dict[str, object]) -> Refusal | None:
    """Admit only the versioned engine's exact exited or restart-backoff flag combinations."""
    state = obj(row["State"])
    code = state.get("ExitCode")
    pid = state.get("Pid")
    if not (
        state.get("Dead") is False
        and state.get("Paused") is False
        and state.get("OOMKilled") is False
        and type(pid) is int
        and pid == 0
        and type(code) is int
        and 0 < code <= MAX_CONTAINER_EXIT
    ):
        return None
    if (
        state.get("Status") == "exited"
        and state.get("Running") is False
        and state.get("Restarting") is False
    ):
        return Refusal("exited", code)
    if (
        state.get("Status") == "restarting"
        and state.get("Running") is True
        and state.get("Restarting") is True
    ):
        return Refusal("restarting", code)
    return None


def endpoint_observation(
    identifier: str,
    bridge: Bridge,
    row: dict[str, object],
    network: dict[str, object],
) -> Endpoint:
    """Join actual running container, sole network, IPAM and both endpoint observations."""
    container_identity(identifier, bridge, row)
    settings = obj(row["NetworkSettings"])
    members = obj(settings["Networks"])
    require(set(members) == {bridge.name}, "endpoint_single_network")
    member = obj(members[bridge.name])
    bridge_identity(bridge, network)
    require(member["NetworkID"] == bridge.identifier, "endpoint_network")
    state = obj(row["State"])
    if observed_refusal(row) is not None:
        refuse("endpoint_observed_refusal")
    pid = state.get("Pid")
    require(
        state.get("Running") is True
        and state.get("Status") == "running"
        and state.get("Restarting") is False
        and state.get("Paused") is False
        and state.get("Dead") is False
        and state.get("OOMKilled") is False
        and type(pid) is int
        and pid > 0,
        "endpoint_container",
    )
    endpoint = text(member["EndpointID"])
    require(re.fullmatch(r"[a-f0-9]{64}", endpoint), "endpoint_identity")
    address, prefix, subnet, gateway = bridge_addresses(member, network)
    containers = obj(network["Containers"])
    require(set(containers) == {identifier}, "endpoint_membership")
    peer = obj(containers[identifier])
    require(
        peer["EndpointID"] == endpoint and peer["IPv4Address"] == f"{address}/{prefix}",
        "endpoint_membership",
    )
    return Endpoint(
        identifier,
        bridge,
        address,
        prefix,
        endpoint,
        subnet,
        gateway,
        operational_bindings(settings.get("Ports")),
    )


def owned_endpoint(commands: Commands, identifier: str, bridge: Bridge) -> Endpoint:
    """Query exact actual IDs, with live original body fences before and after daemon reads."""
    commands.control.check()
    require(identifier in commands.owned, "foreign_container")
    row = one_inspection(commands.call("endpoint-container", ["docker", "inspect", identifier]))
    network = one_inspection(
        commands.call("endpoint-network", ["docker", "network", "inspect", bridge.identifier])
    )
    result = endpoint_observation(identifier, bridge, row, network)
    commands.control.check()
    return result


def negative_endpoint(
    commands: Commands, identifier: str, bridge: Bridge
) -> tuple[Endpoint | None, Refusal | None]:
    """Separate exact-owned exited/backoff observations from any live TLS address authority."""
    commands.control.check()
    require(identifier in commands.owned, "foreign_container")
    row = one_inspection(commands.call("negative-inspect", ["docker", "inspect", identifier]))
    network = one_inspection(
        commands.call("negative-network", ["docker", "network", "inspect", bridge.identifier])
    )
    container_identity(identifier, bridge, row)
    bridge_identity(bridge, network)
    require(obj(row["HostConfig"])["NetworkMode"] == bridge.name, "endpoint_network")
    members = obj(obj(row["NetworkSettings"])["Networks"])
    require(
        set(members) == {bridge.name}
        and obj(members[bridge.name])["NetworkID"] == bridge.identifier,
        "endpoint_network",
    )
    refusal = observed_refusal(row)
    if refusal is not None:
        commands.control.check()
        return None, refusal
    target = endpoint_observation(identifier, bridge, row, network)
    commands.control.check()
    return target, None


def tls_failure_category(error: BaseException) -> str:
    """Classify only fixed connection/handshake families, never certificate or socket text."""
    if isinstance(error, TimeoutError):
        return "timeout"
    if isinstance(error, ssl.SSLCertVerificationError):
        return "certificate-verification"
    if isinstance(error, ssl.SSLError):
        return "tls"
    if isinstance(error, OSError):
        return (
            {
                errno.ECONNREFUSED: "connection-refused",
                errno.ECONNRESET: "connection-reset",
                errno.ENETUNREACH: "network-unreachable",
                errno.EHOSTUNREACH: "host-unreachable",
                errno.EADDRNOTAVAIL: "address-unavailable",
                errno.EPERM: "permission",
                errno.EACCES: "permission",
            }.get(error.errno, "io")
            if error.errno is not None
            else "io"
        )
    return "policy" if isinstance(error, PrototypeError) else "unexpected"


def tls_checkpoint(report: Report, checkpoint: str) -> None:
    """Begin this fixed operation without letting a later close replace the first failed stage."""
    report["tlsCheckpoint"] = checkpoint
    report["tlsFailureCategory"] = None


def public_chain(
    connection: ssl.SSLSocket,
    name: Literal["get_verified_chain", "get_unverified_chain"],
) -> list[bytes]:
    """Adapt public Python3.13 APIs while retaining the project's Python3.12 type baseline."""
    method = cast("object", getattr(connection, name, None))
    require(callable(method), "python_public_chain_api_missing")
    value = cast("Callable[[], object]", method)()
    require(type(value) is list, "tls_chain")
    values = cast("list[object]", value)
    require(
        values and len(values) <= MAX_CHAIN and all(type(item) is bytes for item in values),
        "tls_chain",
    )
    return cast("list[bytes]", values)


@dataclass
class TlsAttempt:
    """Share one subordinate phase end through socket I/O, checked closes and target recheck."""

    target: Endpoint
    report: Report
    end: float
    peers_closed: bool = False
    phase_complete: bool = False
    refused: bool = False
    succeeded: bool = False

    def remaining(self, control: Control) -> float:
        """Fence late completion without resetting the original guest or phase clock."""
        control.check()
        left = self.end - time.monotonic()
        require(left > 0, "tls_deadline")
        return min(control.remaining(3), left)


@dataclass
class TlsPeers:
    """Keep all three admitted socket/HTTP peers reachable across any exchange failure."""

    raw: socket.socket | None = None
    wrapped: ssl.SSLSocket | None = None
    response: http.client.HTTPResponse | None = None

    def close(self, first: BaseException | None) -> tuple[BaseException | None, bool]:
        """Keep earlier exchange failure and separate actual checked-close success."""
        actions = [
            peer.close for peer in (self.response, self.wrapped, self.raw) if peer is not None
        ]
        closing = settle(actions)
        return first or closing, closing is None


def tls_exchange(
    control: Control, attempt: TlsAttempt, wrapped: ssl.SSLSocket, peers: TlsPeers
) -> dict[str, JsonValue]:
    """Use public chain APIs and the exact original TLS-only request/response policy."""
    report = attempt.report
    tls_checkpoint(report, "chain")
    chain = public_chain(wrapped, "get_verified_chain")
    served = public_chain(wrapped, "get_unverified_chain")
    require(len(served) in (2, MAX_CHAIN), "tls_served_chain")
    tls_checkpoint(report, "request")
    wrapped.settimeout(attempt.remaining(control))
    wrapped.sendall(
        b"GET /metrics HTTP/1.1\r\nHost: maintain.localhost\r\nConnection: close\r\n\r\n"
    )
    tls_checkpoint(report, "response")
    wrapped.settimeout(attempt.remaining(control))
    peers.response = http.client.HTTPResponse(wrapped)
    peers.response.begin()
    wrapped.settimeout(attempt.remaining(control))
    require(
        peers.response.status == TLS_ONLY_STATUS and len(peers.response.read(65537)) <= CHUNK,
        "tls_only_response",
    )
    _ = attempt.remaining(control)
    return {
        "version": wrapped.version(),
        "verifiedChainSha256": [hashlib.sha256(item).hexdigest() for item in chain],
        "servedChainSha256": [hashlib.sha256(item).hexdigest() for item in served],
        "status": 404,
    }


def tls_probe(
    control: Control, context: ssl.SSLContext, hostname: str, attempt: TlsAttempt
) -> dict[str, JsonValue]:
    """Keep exact SNI/Host/trust while connecting only to the admitted owned bridge address."""
    report = attempt.report
    attempt.peers_closed = False
    attempt.phase_complete = False
    attempt.refused = False
    attempt.succeeded = False
    _ = attempt.remaining(control)
    peers = TlsPeers()
    first: BaseException | None = None
    result: dict[str, JsonValue] = {}
    try:
        tls_checkpoint(report, "connect")
        peers.raw = socket.create_connection(
            (attempt.target.address, 443), timeout=attempt.remaining(control)
        )
        _ = attempt.remaining(control)
        tls_checkpoint(report, "handshake")
        peers.wrapped = context.wrap_socket(peers.raw, server_hostname=hostname)
        _ = attempt.remaining(control)
        result = tls_exchange(control, attempt, peers.wrapped, peers)
    except BaseException as error:  # noqa: BLE001 -- Settle every known peer.
        first = error
        report["tlsFailureCategory"] = tls_failure_category(error)
    if first is None:
        tls_checkpoint(report, "close")
    first, attempt.peers_closed = peers.close(first)
    try:
        _ = attempt.remaining(control)
    except BaseException as error:  # noqa: BLE001 -- Preserve the first error after all closes.
        first = first or error
    else:
        attempt.phase_complete = True
    if first is not None:
        if report["tlsFailureCategory"] is None:
            report["tlsFailureCategory"] = tls_failure_category(first)
        raise first
    return result


def wait_tls(
    commands: Commands, context: ssl.SSLContext, target: Endpoint, report: Report
) -> dict[str, JsonValue]:
    """Retry the freshly joined immutable target; recheck metadata once the phase completes."""
    end = time.monotonic() + commands.control.remaining(30)
    attempt = TlsAttempt(target, report, end)
    while True:
        commands.control.check()
        if time.monotonic() >= end:
            target.check(commands)
            refuse("tls_not_ready")
        _ = attempt.remaining(commands.control)
        require(target.container in commands.owned, "foreign_container")
        try:
            result = tls_probe(commands.control, context, DOMAIN, attempt)
        except (ConnectionError, ssl.SSLError):
            if not attempt.peers_closed or not attempt.phase_complete:
                raise
            attempt.refused = True
            if time.monotonic() >= end:
                target.check(commands)
                refuse("tls_not_ready")
            commands.control.check()
            time.sleep(min(0.1, max(0, end - time.monotonic())))
        else:
            attempt.succeeded = True
            _ = attempt.remaining(commands.control)
            target.check(commands)
            _ = attempt.remaining(commands.control)
            return result


def profile(commands: Commands, identifier: str, data: Path, config: Path, bridge: str) -> None:
    """Check actual runtime metadata rather than equating create argv with observed policy."""
    rows = array(decode(commands.call("container-profile", ["docker", "inspect", identifier])))
    require(type(rows) is list and len(rows) == 1, "container_profile")
    row = obj(rows[0])
    host = obj(row["HostConfig"])
    config_row = obj(row["Config"])
    require(
        row["Id"] == identifier
        and config_row["User"] == "10001:10001"
        and host["ReadonlyRootfs"] is True
        and host["NetworkMode"] == bridge
        and host["Memory"] == 256 * 1024**2
        and host["PidsLimit"] == PIDS
        and host["CapDrop"] == ["ALL"]
        and host["CapAdd"] == ["NET_BIND_SERVICE"]
        and host["Init"] is True
        and "no-new-privileges" in array(host["SecurityOpt"]),
        "container_profile",
    )
    expected = {
        "/etc/caddy/Caddyfile": (str(commands.root / "Caddyfile"), False),
        "/data": (str(data), True),
        "/config": (str(config), True),
        "/run/simplestchat-browser-assets": (
            str(commands.root / "browser-assets"),
            False,
        ),
    }
    mounts = {
        text(obj(item)["Destination"]): (obj(item)["Source"], obj(item)["RW"])
        for item in array(row["Mounts"])
    }
    require(
        all(mounts.get(name) == value for name, value in expected.items()),
        "container_mounts",
    )
    require(config_row["Image"] == IMAGE, "container_image")
    require(
        host["NanoCpus"] == FIXTURE_NANO_CPUS
        and obj(host["RestartPolicy"]) == {"Name": "unless-stopped", "MaximumRetryCount": 0}
        and config_row["StopTimeout"] == NORMAL_STOP_SECONDS
        and obj(host["Tmpfs"]) == {CONTAINER_TMP: "rw,nosuid,nodev,noexec,size=32m,mode=1777"}
        and obj(host["Sysctls"]) == {"net.ipv4.ip_unprivileged_port_start": "0"}
        and obj(host["LogConfig"])
        == {"Type": "local", "Config": {"max-size": "10m", "max-file": "3"}}
        and obj(host["PortBindings"])
        == {
            "80/tcp": [{"HostIp": "127.0.0.1", "HostPort": "80"}],
            "443/tcp": [{"HostIp": "127.0.0.1", "HostPort": "443"}],
            "443/udp": [{"HostIp": "127.0.0.1", "HostPort": "443"}],
        },
        "container_runtime_profile",
    )
    require(
        len(mounts) == len(expected)
        and all(obj(item).get("Type") == "bind" for item in array(row["Mounts"])),
        "container_mount_types",
    )


def bind_live_chain(observed: dict[str, JsonValue], materials: dict[str, JsonValue]) -> None:
    """Bind both public live chains to the actual three held certificate originals."""
    verified = [
        obj(materials[name])["certificateSha256"] for name in ("leaf", "intermediate", "root")
    ]
    require(observed.get("verifiedChainSha256") == verified, "held_tls_chain")
    served = observed.get("servedChainSha256")
    require(served == verified[:2] or served == verified, "held_served_chain")


def checked_tls(
    commands: Commands, context: ssl.SSLContext, target: Endpoint, report: Report
) -> dict[str, JsonValue]:
    """Recheck the same owned endpoint before and after a separate trusted TLS operation."""
    attempt = TlsAttempt(target, report, time.monotonic() + commands.control.remaining(30))
    target.check(commands)
    observed = tls_probe(commands.control, context, DOMAIN, attempt)
    target.check(commands)
    _ = attempt.remaining(commands.control)
    return observed


def negative_tls(
    commands: Commands, context: ssl.SSLContext, hostname: str, target: Endpoint, report: Report
) -> None:
    """Observe only SSL refusal, with endpoint identity rechecked around the actual probe."""
    attempt = TlsAttempt(target, report, time.monotonic() + commands.control.remaining(30))
    target.check(commands)
    try:
        _ = tls_probe(commands.control, context, hostname, attempt)
    except ssl.SSLError:
        if not attempt.peers_closed or not attempt.phase_complete:
            raise
        commands.control.check()
        target.check(commands)
        _ = attempt.remaining(commands.control)
        return
    refuse("negative_tls_accepted")


def blocked_counter(data: bytes, table: str) -> tuple[int, int]:
    """Read exactly one named counter from the fixed owned inet table."""
    values = obj(decode(data))
    counters = [
        obj(obj(row)["counter"]) for row in array(values["nftables"]) if "counter" in obj(row)
    ]
    blocked = [row for row in counters if row.get("name") == "blocked_output"]
    require(len(blocked) == 1, "guard_observation")
    value = blocked[0]
    packets, size = value.get("packets"), value.get("bytes")
    require(
        value.get("family") == "inet"
        and value.get("table") == table
        and type(packets) is int
        and type(size) is int
        and 0 <= packets <= MAX_COUNTER
        and 0 <= size <= MAX_COUNTER,
        "guard_observation",
    )
    return cast("int", packets), cast("int", size)


def guard_probe(commands: Commands, table: str, report: Report | None = None) -> None:
    """Prove this marker's fresh OUTPUT denial; no real public service is addressed."""
    require(commands.watch is not None, "guard_capture_missing")
    if commands.watch is not None:
        commands.watch.check()
    io_checkpoint(report, "containment-counter-before")
    observed = commands.call("guard-before", ["nft", "-json", "list", "table", "inet", table])
    require(b'"drop"' in observed, "guard_observation")
    before = blocked_counter(observed, table)
    io_checkpoint(report, "containment-marker-open")
    source = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    first = None
    try:
        commands.control.check()
        io_checkpoint(report, "containment-marker-send")
        try:
            sent = source.sendto(MARKER, ("192.0.2.1", 53))
        except OSError as error:
            # Ordinary Linux LOCAL_OUT NF_DROP returns EPERM. Only this exact send may refuse
            # that way, and it is not accepted until fresh counter/capture checks below pass.
            if error.errno != errno.EPERM:
                raise
        else:
            require(sent == len(MARKER), "guard_marker_short")
        commands.control.check()
    except BaseException as error:  # noqa: BLE001 -- Retire the actual socket after any refusal.
        first = error
    if first is None:
        io_checkpoint(report, "containment-marker-close")
    first = settle((source.close,), first)
    if first is not None:
        raise first
    io_checkpoint(report, "containment-counter-after")
    observed = commands.call("guard-after", ["nft", "-json", "list", "table", "inet", table])
    io_checkpoint(report, "containment-counter-validate")
    after = blocked_counter(observed, table)
    require(after[0] == before[0] + 1 and after[1] > before[1], "guard_not_effective")
    io_checkpoint(report, "containment-capture-check")
    if commands.watch is not None:
        commands.watch.check()


def binary_digest(commands: Commands, identifier: str) -> str:
    """Hash actual retained image bytes through mature TAR, never extract an arbitrary pathname."""
    data = commands.call(
        "binary-copy",
        ["docker", "cp", identifier + ":/usr/bin/caddy", "-"],
        30,
        maximum=128 * 1024**2,
    )
    archive = tarfile.open(  # noqa: SIM115 -- Preserve first cause through close.
        fileobj=io.BytesIO(data), mode="r:"
    )
    first, digest = None, ""
    try:
        member = archive.next()
        require(
            member is not None
            and member.isreg()
            and member.name == "caddy"
            and 0 < member.size <= 128 * 1024**2,
            "binary_member",
        )
        member = cast("tarfile.TarInfo", member)
        stream = archive.extractfile(member)
        require(stream is not None, "binary_member")
        if stream is not None:
            body = None
            try:
                body = stream.read(128 * 1024**2 + 1)
                require(len(body) == member.size, "binary_size")
                digest = hashlib.sha256(body).hexdigest()
            finally:
                stream.close()
        require(archive.next() is None, "binary_members")
    except BaseException as error:  # noqa: BLE001 -- Keep first refusal through archive close.
        first = error
    first = settle((archive.close,), first)
    if first is not None:
        raise first
    return digest


def negative_copy_outcome(
    commands: Commands, target: Endpoint | None, report: Report, initial: Refusal | None
) -> tuple[str, Refusal | None]:
    """Use one negative-copy phase and one fresh state join only after clean retry exhaustion."""
    if target is None:
        return "refused-or-not-ready", initial
    context = ssl.create_default_context(cafile=str(commands.root / "data/caddy" / MATERIALS[0]))
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    attempt = TlsAttempt(target, report, time.monotonic() + commands.control.remaining(30))
    while True:
        commands.control.check()
        if time.monotonic() >= attempt.end:
            require(
                attempt.refused
                and attempt.peers_closed
                and attempt.phase_complete
                and not attempt.succeeded,
                "tls_deadline",
            )
            current, refusal = negative_endpoint(commands, target.container, target.bridge)
            if current is not None:
                require(current == target, "endpoint_changed")
            commands.control.check()
            return "refused-or-not-ready", refusal
        try:
            probe = tls_probe(commands.control, context, DOMAIN, attempt)
        except (ConnectionError, ssl.SSLError):
            if not attempt.peers_closed or not attempt.phase_complete:
                raise
            attempt.refused = True
            commands.control.check()
            time.sleep(min(0.1, max(0, attempt.end - time.monotonic())))
        else:
            attempt.succeeded = True
            _ = attempt.remaining(commands.control)
            target.check(commands)
            _ = attempt.remaining(commands.control)
            require(probe["status"] == TLS_ONLY_STATUS, "negative_tls_status")
            return "tls-after-perturbation", None


def negative_copies(  # noqa: C901 -- Fixed two-copy experiment and custody.
    commands: Commands,
    originals: Sequence[Original],
    bridge: Bridge,
    report: Report,
) -> list[dict[str, JsonValue]]:
    """Perturb owned copies only; distinguish refusal from regeneration, never original reuse."""
    rows: list[dict[str, JsonValue]] = []
    for scenario in ("missing-leaf-key", "corrupt-issuer-data"):
        for original in originals:
            original.check(commands.control)
        base = commands.root / scenario
        base.mkdir(mode=0o700)
        data, config = base / "data", base / "config"
        data.mkdir(mode=0o700)
        config.mkdir(mode=0o700)
        os.chown(data, 10001, 10001)
        os.chown(config, 10001, 10001)
        # Copy only the exact seven bounded originals; no unbounded auxiliary traversal.
        for name, original in zip(MATERIALS, originals, strict=True):
            commands.control.check()
            destination = data / "caddy" / name
            lineage: list[Path] = []
            current = destination.parent
            while current != data:
                lineage.append(current)
                current = current.parent
            for parent in reversed(lineage):
                if not parent.exists():
                    parent.mkdir(mode=0o700)
                    os.chown(parent, 10001, 10001)
            write_file(destination, original.data)
            os.chown(destination, 10001, 10001)
        target = data / "caddy" / MATERIALS[5 if scenario == "missing-leaf-key" else 6]
        target.unlink()
        if scenario == "corrupt-issuer-data":
            write_file(
                target,
                b'{"sans":["maintain.localhost"],"issuer_data":{"invalid":true}}',
            )
            os.chown(target, 10001, 10001)
        supplied = {
            name: bounded_original(data / "caddy" / name, CHUNK, commands.control)
            for name in MATERIALS
            if (data / "caddy" / name).is_file()
        }
        try:
            _ = caddy_materials.inspect(supplied, datetime.now(UTC), commands.control.remaining(30))
        except caddy_materials.MaterialError:
            pass
        else:
            refuse("negative_material_accepted")
        identifier = commands.create(
            "negative-create", normal_arguments(commands.root, data, config, bridge.name)
        )
        _ = commands.call("negative-start", ["docker", "start", identifier])
        target, refusal = negative_endpoint(commands, identifier, bridge)
        # A bounded live outcome is observational: Caddy may refuse or legitimately regenerate.
        outcome, refusal = negative_copy_outcome(commands, target, report, refusal)
        _ = commands.call("negative-stop", ["docker", "stop", "--time", "10", identifier], 15)
        commands.remove(identifier)
        changed = [
            name
            for name, original in zip(MATERIALS, originals, strict=True)
            if not (data / "caddy" / name).is_file()
            or bounded_original(data / "caddy" / name, CHUNK, commands.control) != original.data
        ]
        require(scenario != "missing-leaf-key" or changed, "missing_key_not_observed")
        rows.append(
            {
                "phase": scenario,
                "outcome": outcome,
                "changedMaterialCount": len(changed),
                "originalReuseClaimed": False,
                "passiveMaterialsRefused": True,
                "observedExitStatus": refusal.exit_status if refusal is not None else None,
                "observedState": refusal.state if refusal is not None else "running",
                "tlsAttempted": target is not None,
            }
        )
        for original in originals:
            original.check(commands.control)
    return rows


def main(  # noqa: C901, PLR0912, PLR0915 -- Keep one ownership frame.
    arguments: Sequence[str] | None = None,
) -> int:
    """Own guest effects, then restore all process-global state before the final decision."""
    started = time.monotonic()
    parser = argparse.ArgumentParser()
    _ = parser.add_argument("--run-id", required=True)
    _ = parser.add_argument("--seconds", required=True, type=float)
    _ = parser.add_argument("--finalization-seconds", required=True, type=float)
    options = parser.parse_args(arguments, namespace=Options())
    require(re.fullmatch(r"[a-f0-9]{32}", options.run_id), "fixture_identity")
    require(
        math.isfinite(options.seconds)
        and math.isfinite(options.finalization_seconds)
        and 0 < options.finalization_seconds < options.seconds,
        "fixture_budget",
    )
    control = Control(started, options.seconds, options.finalization_seconds)
    previous_mask = os.umask(0o077)
    handlers = {number: signal.getsignal(number) for number in (signal.SIGINT, signal.SIGTERM)}
    first = None
    commands: Commands | None = None
    watch: Watch | None = None
    originals: list[Original] = []
    effects = Effects()
    root_copy: TrustCopy | None = None
    table = "sc_caddy_" + options.run_id[:12]
    bridge = "sc" + options.run_id[:12]
    report: Report = {
        "schemaVersion": 1,
        "scope": "CaddyExactSourceDisposableGuest",
        "passed": False,
        "cleanupPassed": False,
        "failure": None,
        "image": IMAGE,
        "sourceClosure": CLOSURE,
        "materials": {},
        "observations": [],
        "containment": {},
        "qualification": QUALIFICATION,
        "checkpoint": "admission",
        "exception": None,
        "cleanupException": None,
        "ioCheckpoint": None,
        "errnoCategory": None,
        "tlsCheckpoint": None,
        "tlsFailureCategory": None,
    }
    try:
        for number in handlers:
            _ = signal.signal(number, control.interrupt)
        require(os.getuid() == 0 and os.getgid() == 0, "fixture_root")
        require(
            bounded_original(Path("/etc/simplestchat-vm-fixture"), 64, control)
            .decode("ascii")
            .strip()
            == options.run_id,
            "fixture_marker",
        )
        report["checkpoint"] = "guest-prerequisites"
        root = Path("/var/lib/simplestchat-caddy-prototype") / options.run_id
        root.mkdir(parents=True, mode=0o700, exist_ok=False)
        commands = Commands(root, control)
        for name in ("docker", "nft", "tcpdump", "ip", "update-ca-certificates"):
            owner = Tool.open(name, control)
            commands.tool_owners[name] = owner
            commands.tools[name] = str(owner.path)
        prerequisite: dict[str, JsonValue] = {
            "phase": "guest-prerequisites",
            "pythonMajor": sys.version_info.major,
            "pythonMinor": sys.version_info.minor,
            "pythonMicro": sys.version_info.micro,
            "publicVerifiedChainApi": hasattr(ssl.SSLSocket, "get_verified_chain"),
            "publicServedChainApi": hasattr(ssl.SSLSocket, "get_unverified_chain"),
        }
        require(
            sys.version_info >= (3, 13)
            and prerequisite["publicVerifiedChainApi"]
            and prerequisite["publicServedChainApi"],
            "python_public_chain_api_missing",
        )
        report["observations"].append(prerequisite)
        report["checkpoint"] = "source-setup"
        source = Path(__file__).with_name("Caddyfile")
        data = bounded_original(source, CHUNK, control)
        require(hashlib.sha256(data).hexdigest() == CADDYFILE, "caddyfile_source")
        write_file(root / "Caddyfile", data, 0o644)
        write_file(
            root / "proxy.env",
            (
                b"CADDY_DOMAIN=maintain.localhost\nCADDY_UPSTREAM=simplestchat:3000\n"
                b"TRUSTED_PROXY_SECRET=INERT-EXACT-SOURCE-PROTOTYPE-000000000000000000000000\n"
            ),
        )
        for name in ("data", "config", "browser-assets"):
            path = root / name
            path.mkdir(mode=0o700)
            os.chown(path, 10001, 10001)
        report["checkpoint"] = "image"
        _ = commands.call("docker-version", ["docker", "version", "--format", "{{json .}}"])
        _ = commands.call("image-pull", ["docker", "pull", IMAGE], 180)
        image = [
            obj(item)
            for item in array(
                decode(commands.call("image-inspect", ["docker", "image", "inspect", IMAGE]))
            )
        ]
        require(len(image) == 1 and image[0]["Architecture"] == "amd64", "image_platform")
        require(
            {"XDG_DATA_HOME=/data", "XDG_CONFIG_HOME=/config"}.issubset(
                array(obj(image[0]["Config"])["Env"])
            ),
            "image_storage_environment",
        )
        require(
            any(
                text(value).endswith("@" + IMAGE.split("@", 1)[1])
                for value in array(image[0]["RepoDigests"])
            ),
            "image_digest",
        )
        report["observations"].append({"phase": "image", "imageId": text(image[0]["Id"])})

        report["checkpoint"] = "network"

        def network_created(data: bytes) -> None:
            require(re.fullmatch(b"[a-f0-9]{64}\n", data), "network_unknown")
            effects.network_id = data.strip().decode("ascii")

        _ = commands.call(
            "network-create",
            [
                "docker",
                "network",
                "create",
                "--internal",
                "--driver",
                "bridge",
                "--opt",
                "com.docker.network.bridge.name=" + bridge,
                bridge,
            ],
            adopt=network_created,
        )
        require(effects.network_id is not None, "network_unknown")
        transport = Bridge(text(effects.network_id), bridge, text(image[0]["Id"]))
        report["checkpoint"] = "containment"
        report["ioCheckpoint"] = "containment-route"
        routes = array(decode(commands.call("route", ["ip", "-json", "route", "show", "default"])))
        require(type(routes) is list and len(routes) == 1, "physical_interface")
        interface = text(obj(routes[0])["dev"])
        peer = "10.0.2.2"
        rules = firewall(table, interface, bridge, peer)
        report["ioCheckpoint"] = "containment-rule-file"
        write_file(root / "guard.nft", rules.encode())

        def installed(_data: bytes) -> None:
            effects.guarded = True

        report["ioCheckpoint"] = "containment-guard-install"
        _ = commands.call("guard-install", ["nft", "-f", str(root / "guard.nft")], adopt=installed)
        report["ioCheckpoint"] = "containment-capture-start"
        watch = Watch.start(commands, interface, peer)
        commands.watch = watch
        guard_probe(commands, table, report)
        report["ioCheckpoint"] = None

        # No image pull, package manager, resolver grant or upstream request after this point.
        report["checkpoint"] = "module-graph"
        metadata = commands.create(
            "metadata-create",
            [
                "--network",
                "none",
                "--pull",
                "never",
                "--read-only",
                "--user",
                "10001:10001",
                IMAGE,
                "caddy",
                "build-info",
            ],
        )
        _ = commands.call("metadata-start", ["docker", "start", "--attach", metadata])
        info = commands.call("metadata-logs", ["docker", "logs", metadata]).decode("utf-8")
        require(
            re.search(r"github\.com/caddyserver/certmagic\s+v0\.25\.6(?:\s|$)", info),
            "certmagic_version",
        )
        require(
            re.search(r"github\.com/mholt/acmez/v3\s+v3\.1\.7(?:\s|$)", info) and "=>" not in info,
            "acmez_version",
        )
        binary_sha = binary_digest(commands, metadata)

        commands.remove(metadata)
        version = commands.create(
            "version-create",
            [
                "--network",
                "none",
                "--pull",
                "never",
                "--read-only",
                "--user",
                "10001:10001",
                IMAGE,
                "caddy",
                "version",
            ],
        )
        _ = commands.call("version-start", ["docker", "start", "--attach", version])
        version_text = commands.call("version-logs", ["docker", "logs", version])
        require(version_text.split(maxsplit=1)[0] == b"v2.11.7", "caddy_version")
        commands.remove(version)
        modules = commands.create(
            "modules-create",
            [
                "--network",
                "none",
                "--pull",
                "never",
                "--read-only",
                "--user",
                "10001:10001",
                IMAGE,
                "caddy",
                "list-modules",
                "--versions",
            ],
        )
        _ = commands.call("modules-start", ["docker", "start", "--attach", modules])
        module_bytes = commands.call("modules-logs", ["docker", "logs", modules])
        module_lines = module_bytes.decode("utf-8").splitlines()
        require(
            any(
                line.split(maxsplit=1)[0] == "tls.issuance.internal"
                for line in module_lines
                if line.split()
            ),
            "internal_module_missing",
        )
        commands.remove(modules)
        report["observations"].append(
            {
                "phase": "module-graph",
                "caddyVersion": "v2.11.7",
                "buildInfoSha256": hashlib.sha256(info.encode()).hexdigest(),
                "binarySha256": binary_sha,
                "registeredModulesSha256": hashlib.sha256(module_bytes).hexdigest(),
                "registeredModuleLines": len(module_lines),
            }
        )
        report["checkpoint"] = "natural-tls"
        identifier = commands.create(
            "natural-create",
            normal_arguments(root, root / "data", root / "config", bridge),
        )
        profile(commands, identifier, root / "data", root / "config", bridge)
        _ = commands.call("natural-start", ["docker", "start", identifier])
        # Temporary explicit trust accepts only the public root actually generated in this attempt.
        ca = root / "data/caddy/pki/authorities/local/root.crt"
        ready_end = time.monotonic() + control.remaining(30)
        while not ca.exists():
            require(time.monotonic() < ready_end, "natural_ca_not_ready")
            watch.check()
            time.sleep(0.05)
        originals.append(Original.open(ca, control))
        public_context = ssl.create_default_context(cadata=originals[0].data.decode("ascii"))
        public_context.minimum_version = ssl.TLSVersion.TLSv1_2
        target = owned_endpoint(commands, identifier, transport)
        first_tls = wait_tls(commands, public_context, target, report)
        # Keep each admitted original registered if a later material refuses.
        for relative in MATERIALS[1:]:
            originals.append(  # noqa: PERF401 -- Keep each admitted owner.
                Original.open(root / "data/caddy" / relative, control)
            )
        report["materials"] = caddy_materials.inspect(
            dict(zip(MATERIALS, (item.data for item in originals), strict=True)),
            datetime.now(UTC),
            control.end - time.monotonic(),
        )
        report["checkpoint"] = "trust-install"
        candidate_root = Path("/usr/local/share/ca-certificates") / (table + ".crt")
        root_copy = TrustCopy.create(candidate_root, originals[0].data, control)
        _ = commands.call("install-public-root", ["update-ca-certificates"])
        trusted = ssl.create_default_context()
        trusted.minimum_version = ssl.TLSVersion.TLSv1_2
        trusted_tls = checked_tls(commands, trusted, target, report)
        require(trusted_tls == first_tls, "tls_chain")
        bind_live_chain(trusted_tls, report["materials"])
        _ = commands.call("natural-stop", ["docker", "stop", "--time", "10", identifier], 15)
        commands.remove(identifier)
        for original in originals:
            original.check(control)
        report["checkpoint"] = "fresh-process"
        fresh = commands.create(
            "fresh-create",
            normal_arguments(root, root / "data", root / "config", bridge),
        )
        profile(commands, fresh, root / "data", root / "config", bridge)
        _ = commands.call("fresh-start", ["docker", "start", fresh])
        fresh_target = owned_endpoint(commands, fresh, transport)
        repeated = wait_tls(commands, trusted, fresh_target, report)
        require(repeated == trusted_tls, "fresh_tls_changed")
        for original in originals:
            original.check(control)
        report["checkpoint"] = "negative-tls"
        empty = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        empty.minimum_version = ssl.TLSVersion.TLSv1_2
        negative_tls(commands, empty, DOMAIN, fresh_target, report)
        negative_tls(commands, trusted, "wrong.maintain.localhost", fresh_target, report)
        report["observations"].extend(
            [
                {"phase": "natural-tls", **trusted_tls, **target.facts()},
                {"phase": "fresh-process-same-seven", **repeated, **fresh_target.facts()},
                {"phase": "wrong-trust-and-hostname", "refused": True},
            ]
        )
        _ = commands.call("fresh-stop", ["docker", "stop", "--time", "10", fresh], 15)
        commands.remove(fresh)
        report["checkpoint"] = "validator"
        report["observations"].append(validate(commands))
        report["checkpoint"] = "negative-copies"
        report["observations"].extend(negative_copies(commands, originals, transport, report))
        report["checkpoint"] = "final-observations"
        for original in originals:
            original.check(control)
        watch.check()
        counters = decode(
            commands.call("guard-observe", ["nft", "-json", "list", "table", "inet", table])
        )
        report["containment"] = {
            "guard": "guest-output-and-forward-default-drop",
            "dnsForwardingCovered": True,
            "guardRetainedUntilVmTeardown": True,
            "wireNoncontrolPackets": 0,
            "linkGuard": "physical-netdev-egress-control-peer-only",
            "nftObservationSha256": hashlib.sha256(json.dumps(counters).encode()).hexdigest(),
        }
    except BaseException as error:  # noqa: BLE001 -- Settle every known peer.
        first = error
        report["exception"] = exception_kind(error)
        report["errnoCategory"] = errno_category(error)
    finally:
        # Preserve the last body-phase refusal before admitting only bounded cleanup.
        checked = settle((control.check,), first)
        if first is None and checked is not None:
            report["checkpoint"] = "body-completion"
            report["exception"] = exception_kind(checked)
            report["errnoCategory"] = errno_category(checked)
        first = checked
        control.settling = True
        actions: list[Callable[[], object]] = []
        if commands is not None:
            actions.extend(
                lambda identifier=identifier, owner=commands: owner.remove(identifier)
                for identifier in tuple(commands.owned)
            )
        actions.extend(lambda item=item: item.check(control) for item in originals)
        actions.extend(item.close for item in originals)
        if watch is not None:
            actions.append(watch.finish)
        if commands is not None and effects.guarded:
            actions.append(lambda owner=commands: check_guard_retained(owner, "netdev", table))
            actions.append(lambda owner=commands: check_guard_retained(owner, "inet", table))
        if commands is not None and effects.network_id is not None:
            actions.append(
                lambda owner=commands: owner.call(
                    "network-remove",
                    ["docker", "network", "rm", cast("str", effects.network_id)],
                )
            )
        if root_copy is not None:
            actions.append(lambda owner=root_copy: owner.finish(control))
            if commands is not None:
                actions.append(
                    lambda owner=commands: owner.call(
                        "remove-public-root", ["update-ca-certificates"]
                    )
                )
        if commands is not None:
            actions.extend(
                lambda owner=owner: owner.finish(control) for owner in commands.tool_owners.values()
            )
        cleanup_error = settle(actions)
        if first is None and cleanup_error is not None:
            report["checkpoint"] = "cleanup"
            report["exception"] = exception_kind(cleanup_error)
            report["errnoCategory"] = errno_category(cleanup_error)
        report["cleanupException"] = exception_kind(cleanup_error)
        first = first or cleanup_error
        report["cleanupPassed"] = cleanup_error is None
        restored = settle(tuple(lambda n=n, h=h: signal.signal(n, h) for n, h in handlers.items()))
        if first is None and restored is not None:
            report["checkpoint"] = "restore"
            report["exception"] = exception_kind(restored)
            report["errnoCategory"] = errno_category(restored)
        first = first or restored
        try:
            _ = os.umask(previous_mask)
            control.check()
            require(not control.cancelled, "guest_cancelled")
        except BaseException as error:  # noqa: BLE001 -- Settle every known peer.
            if first is None:
                report["checkpoint"] = "final-fence"
                report["exception"] = exception_kind(error)
                report["errnoCategory"] = errno_category(error)
            first = first or error
    report["passed"] = first is None
    if first is None:
        report["checkpoint"] = "complete"
    if first is not None:
        report["failure"] = str(first) if type(first) is PrototypeError else "prototype_failed"
    encoded = (json.dumps(report, sort_keys=True, separators=(",", ":")) + "\n").encode()
    require(len(encoded) <= MAX_METADATA, "metadata_bound")
    require(sys.stdout.buffer.write(encoded) == len(encoded), "metadata_output_short")
    _ = sys.stdout.buffer.flush()
    control.check()
    return 1 if first is not None or control.cancelled else 0


if __name__ == "__main__":
    raise SystemExit(main())
