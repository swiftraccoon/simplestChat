"""TEST ONLY: finite GNU-timeout topology probes inside exclusively owned PID namespaces.

The controller, PID1 and inert leaf are fixed roles, not command dispatchers.
This file must never replace or execute the operational maintenance launcher.
"""

from __future__ import annotations

import contextlib
import errno
import fcntl
import hashlib
import io
import json
import math
import os
import platform
import re
import select
import selectors
import signal
import socket
import stat
import struct
import subprocess
import sys
import time
from pathlib import Path
from typing import TYPE_CHECKING, cast

# The isolated guest contains only this source-bound helper directory.
sys.path.insert(0, str(Path(__file__).resolve().parent))
import bounded_process

if TYPE_CHECKING:
    from collections.abc import Callable, Sequence
    from typing import BinaryIO

CASES = (
    "natural-zero",
    "completed-nonzero",
    "default-timeout-pgid",
    "term-during-inner",
    "terminal-leader-live-descendant",
    "eof-with-live-descendant",
    "ignore-term",
    "exit-trap-new-child",
    "late-input-or-signal",
    "helper-or-close-failure",
    "host-proc-netns-role",
)
CASE_VARIANTS = {
    "late-input-or-signal": ("expired", "eof", "signal"),
    "helper-or-close-failure": ("ordinary", "truncated"),
}
MAX_FRAME = 16384
MAX_STREAM = 65536
MAX_EVENTS = 64
CASE_SECONDS = 30
TERM_SECONDS = 5
REAP_SECONDS = 5
PRIVATE_MODE = 0o600
MINIMUM_WORK = 100
BODY_SECONDS = 1680
MAX_EVENT_BYTES = 1024
LEAF_ARGUMENTS = 4
INIT_ARGUMENTS = 8
GUEST_ARGUMENTS = 2
ROLE_ARGUMENTS = 2
EXIT_FAILURE = 7
INTERRUPTED_STATUS = 130
PIDFD_CAPABILITIES = 2
NS_GET_NSTYPE = 0xB703
CLONE_NEWPID = 0x20000000
READY_KEYS = frozenset({"kind", "nonce", "hostPid", "pid", "namespace", "pidfd", "proc"})
GO_KEYS = frozenset({"kind", "nonce", "bodyEnd", "finalEnd"})
SELF = Path(__file__).resolve()
PYTHON = "/usr/bin/python3"
TIMEOUT = "/usr/bin/timeout"
UNSHARE = "/usr/bin/unshare"
BASH = "/usr/bin/bash"
TOOLS = (TIMEOUT, UNSHARE, BASH, "/usr/bin/dpkg-query")
ENV = {"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LC_ALL": "C", "HOME": "/root"}
WORKER = r"""
set -eu
case_name=$1; script=$2; event_fd=$3; command_fd=$4; end=$5
if [ "$case_name" = exit-trap-new-child ]; then
  cleanup() {
    /usr/bin/timeout --signal=TERM --kill-after=2s 15s \
      /usr/bin/python3 -I -B "$script" leaf cleanup "$event_fd" "$command_fd" "$end"
  }
  trap cleanup EXIT
fi
trap 'exit 143' TERM
trap 'exit 130' INT
/usr/bin/timeout --signal=TERM --kill-after=5s 180s \
  /usr/bin/python3 -I -B "$script" leaf "$case_name" "$event_fd" "$command_fd" "$end"
"""


class ProbeError(ValueError):
    """A fixed private probe refusal, never a raw tool error or credential value."""


def require(value: object, code: str) -> None:
    """Reject a missing proof without formatting external content."""
    if not value:
        raise ProbeError(code)


def remember(
    actions: Sequence[Callable[[], None]], first: BaseException | None = None
) -> BaseException | None:
    """Attempt every admitted close while retaining the first observed object."""
    for action in actions:
        try:
            action()
        except BaseException as error:  # noqa: BLE001 -- All peers close after any refusal.
            if first is None:
                first = error
    return first


def exception_kind(error: BaseException | None) -> str | None:
    """Classify a fixed family without exposing any error text, class name or arguments."""
    if error is None:
        return None
    catalog = (
        (TimeoutError, "timeout"),
        (KeyboardInterrupt, "signal"),
        (ProbeError, "policy"),
        ((ValueError, UnicodeError), "decode"),
        (OSError, "io"),
    )
    for family, kind in catalog:
        if isinstance(error, family):
            return kind
    return "unexpected"


def number(value: object) -> int:
    """Accept only an actual positive bounded kernel identity."""
    require(type(value) is int and 0 < value < 2**31, "identity")
    return cast("int", value)


def stamp(value: os.stat_result) -> tuple[int, ...]:
    """Compare immutable identity without treating read-induced atime as replacement."""
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


def object_json(data: bytes) -> dict[str, object]:
    """Parse a small unique-key finite object, rejecting unknown container keys later."""

    def pairs(items: list[tuple[str, object]]) -> dict[str, object]:
        result: dict[str, object] = {}
        for key, value in items:
            require(key not in result, "duplicate-key")
            result[key] = value
        return result

    def constant(_: str) -> object:
        message = "nonfinite-json"
        raise ProbeError(message)

    def finite(text: str) -> float:
        result = float(text)
        require(math.isfinite(result), "nonfinite-json")
        return result

    require(0 < len(data) <= MAX_FRAME, "frame-size")
    value = cast(
        "object",
        json.loads(data, object_pairs_hook=pairs, parse_constant=constant, parse_float=finite),
    )
    require(isinstance(value, dict), "frame-object")
    return cast("dict[str, object]", value)


def encode(value: dict[str, object]) -> bytes:
    """Encode one bounded closed frame without a text stream owner."""
    raw = json.dumps(value, separators=(",", ":"), allow_nan=False).encode()
    require(len(raw) <= MAX_FRAME, "frame-size")
    return raw


def packet(channel: socket.socket, value: dict[str, object], fds: Sequence[int] = ()) -> None:
    """Transfer fixed kernel capabilities with one atomic seqpacket frame."""
    raw = encode(value)
    rights = (
        []
        if not fds
        else [(socket.SOL_SOCKET, socket.SCM_RIGHTS, struct.pack(f"{len(fds)}i", *fds))]
    )
    require(channel.sendmsg([raw], rights) == len(raw), "short-frame")


def retire_rights(descriptors: Sequence[int]) -> tuple[BaseException | None, bool]:
    """Retire every received right and collect EBADF facts before another admission."""
    first = remember(tuple(lambda fd=fd: os.close(fd) for fd in descriptors))
    actual_closed = first is None
    for fd in descriptors:
        try:
            _ = os.fstat(fd)
        except OSError as error:
            actual_closed = actual_closed and error.errno == errno.EBADF
        else:
            actual_closed = False
    return first, actual_closed


def receive(
    channel: socket.socket,
    end: float,
    *,
    allow_eof: bool = False,
    refusals: list[dict[str, object]] | None = None,
) -> tuple[dict[str, object], list[int], int | None]:
    """Keep received descriptors owned even when a malformed frame refuses."""
    left = end - time.monotonic()
    require(left > 0, "frame-deadline")
    channel.settimeout(left)
    received = channel.recvmsg(MAX_FRAME, socket.CMSG_SPACE(12) * 2)
    data, ancillary, flags = received[:3]
    fds: list[int] = []
    sender: int | None = None
    for level, kind, raw in ancillary:
        if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
            width = struct.calcsize("i")
            complete = raw[: len(raw) - len(raw) % width]
            fds.extend(struct.unpack(f"{len(complete) // width}i", complete))
    try:
        for fd in fds:
            os.set_inheritable(fd, False)  # noqa: FBT003 -- Stdlib positional-only flag.
        require(time.monotonic() < end, "frame-deadline")
        credentials = 0
        for level, kind, raw in ancillary:
            require(level == socket.SOL_SOCKET, "ancillary-level")
            require(kind in (socket.SCM_RIGHTS, socket.SCM_CREDENTIALS), "ancillary-kind")
            if kind == socket.SCM_RIGHTS:
                require(len(raw) % struct.calcsize("i") == 0, "rights-size")
            elif kind == socket.SCM_CREDENTIALS:
                credentials += 1
                require(credentials == 1 and len(raw) == struct.calcsize("3i"), "credentials-size")
                sender, uid, gid = struct.unpack("3i", raw)
                require(uid == 0 and gid == 0, "credentials-owner")
        if allow_eof and not data and not ancillary and flags == 0:
            return {"kind": "channel-eof"}, fds, sender
        require(flags == 0 and data, "frame-eof-or-truncation")
        return object_json(data), fds, sender
    except BaseException as error:
        closed, actual_closed = retire_rights(fds)
        if refusals is not None:
            refusals.append(
                {
                    "rightsReceived": len(fds),
                    "allRightsClosed": actual_closed,
                    "truncated": bool(flags & (socket.MSG_TRUNC | socket.MSG_CTRUNC)),
                }
            )
        # The observed frame error stays primary; close failure remains a separate negative fact.
        raise error from closed


def kernel_status() -> dict[str, object]:
    """Read numeric identity fields only through the unchanged host proc mount."""
    with Path("/proc/self/status").open("rb") as source:
        raw = source.read(MAX_STREAM + 1)
    require(len(raw) <= MAX_STREAM, "proc-size")
    fields = dict(line.split(":", 1) for line in raw.decode().splitlines() if ":" in line)
    ids = [int(item) for item in fields["NSpid"].split()]
    return {
        "hostPid": number(ids[0]),
        "pid": os.getpid(),
        "parentHostPid": int(fields["PPid"].strip()),
        "pgid": os.getpgrp(),
        "sid": os.getsid(0),
        "namespaceDepth": len(ids) - 1,
    }


def identity_packet(channel: socket.socket, value: dict[str, object]) -> None:
    """Transfer this sender's actual original namespace and self pidfd, never a PID hint."""
    owned: list[int] = []
    first: BaseException | None = None
    try:
        namespace = os.open("/proc/self/ns/pid", os.O_RDONLY | os.O_CLOEXEC)
        owned.append(namespace)
        if hasattr(os, "pidfd_open"):
            owned.append(os.pidfd_open(os.getpid()))
        value.update(
            {"namespace": os.fstat(namespace).st_ino, "pidfd": len(owned) == PIDFD_CAPABILITIES}
        )
        packet(channel, value, owned)
    except BaseException as error:  # noqa: BLE001 -- Settle every original capability.
        first = error
    finally:
        first = remember(tuple(lambda fd=fd: os.close(fd) for fd in owned), first)
    if first is not None:
        raise first


def proc_bytes(proc_fd: int, name: str) -> bytes:
    """Read a bounded kernel record through the original host-proc directory owner."""
    descriptor = os.open(name, os.O_RDONLY | os.O_CLOEXEC, dir_fd=proc_fd)
    first: BaseException | None = None
    result = bytearray()
    try:
        while raw := os.read(descriptor, 4096):
            result.extend(raw)
            require(len(result) <= MAX_STREAM, "proc-size")
    except BaseException as error:  # noqa: BLE001 -- Keep the observed read before close.
        first = error
    finally:
        first = remember((lambda: os.close(descriptor),), first)
    if first is not None:
        raise first
    return bytes(result)


def pidfd_live(descriptor: int) -> None:
    """Guard a kernel-derived sender lookup with the same original pidfd on both sides."""
    require(hasattr(signal, "pidfd_send_signal"), "pidfd-signal-zero-api")
    signal.pidfd_send_signal(descriptor, 0)
    require(not select.select([descriptor], [], [], 0)[0], "pidfd-live-before-go")


def identity_mapping(
    value: dict[str, object],
    descriptors: Sequence[int],
    sender: int | None,
    proc_fd: int,
    *,
    expect_init: bool = False,
) -> dict[str, object]:
    """Join actual PID-namespace rights to the live kernel sender, never reported PID authority."""
    require(sender is not None and value.get("pidfd") is True, "identity-credentials")
    require(len(descriptors) == PIDFD_CAPABILITIES, "identity-capabilities")
    namespace, pidfd = descriptors
    require(fcntl.ioctl(namespace, NS_GET_NSTYPE) == CLONE_NEWPID, "namespace-kind")
    observed = os.fstat(namespace)
    identity = (observed.st_dev, observed.st_ino)
    require(value.get("namespace") == observed.st_ino, "identity-namespace")
    pidfd_live(pidfd)
    raw = proc_bytes(proc_fd, f"self/fdinfo/{pidfd}")
    fields = dict(line.split(":", 1) for line in raw.decode().splitlines() if ":" in line)
    require("NSpid" in fields, "pidfd-info-mapping")
    ids = [number(int(item)) for item in fields["NSpid"].split()]
    receiver = proc_bytes(proc_fd, "self/status")
    own_fields = dict(line.split(":", 1) for line in receiver.decode().splitlines() if ":" in line)
    depth = len(own_fields["NSpid"].split()) - 1
    require(depth < len(ids) and ids[depth] == sender, "pidfd-credential-mapping")
    if expect_init:
        require(len(ids) == depth + 2 and ids[-1] == 1, "pidfd-namespace-init")
    # ids[0] comes only from the retained pidfd's kernel record. JSON PIDs are never opened.
    original = os.open(f"{ids[0]}/ns/pid", os.O_RDONLY | os.O_CLOEXEC, dir_fd=proc_fd)
    first: BaseException | None = None
    try:
        require(fcntl.ioctl(original, NS_GET_NSTYPE) == CLONE_NEWPID, "sender-namespace-kind")
        actual = os.fstat(original)
        require((actual.st_dev, actual.st_ino) == identity, "sender-namespace-join")
        pidfd_live(pidfd)
    except BaseException as error:  # noqa: BLE001 -- All admitted namespace owners close.
        first = error
    finally:
        first = remember((lambda: os.close(original),), first)
    if first is not None:
        raise first
    pidfd_live(pidfd)
    return {
        "namespaceMapped": True,
        "namespaceKindPid": True,
        "senderNamespaceJoined": True,
        "pidfdMapped": True,
        "signalZeroPassed": True,
        "liveBeforeGo": True,
        "receiverNamespaceDepth": depth,
    }


def wrong_capabilities(
    channels: tuple[socket.socket, socket.socket], original: int, pidfd: int, nonce: str, end: float
) -> None:
    """Send real self pidfd with two wrong namespace roles before any work admission."""
    ready, go = channels
    for role in ("netns", "parent-pidns"):
        require(time.monotonic() < end, "negative-capability-admission")
        namespace = (
            os.open("/proc/self/ns/net", os.O_RDONLY | os.O_CLOEXEC)
            if role == "netns"
            else os.dup(original)
        )
        first: BaseException | None = None
        try:
            packet(
                ready,
                {
                    "kind": "namespace-negative",
                    "nonce": nonce,
                    "role": role,
                    "namespace": os.fstat(namespace).st_ino,
                    "pidfd": True,
                },
                (namespace, pidfd),
            )
        except BaseException as error:  # noqa: BLE001 -- Retain body while closing original.
            first = error
        finally:
            first = remember((lambda fd=namespace: os.close(fd),), first)
        if first is not None:
            raise first
        response, rights, _sender = receive(go, end)
        close_error, closed = retire_rights(rights)
        require(close_error is None and closed and not rights, "negative-ack-rights")
        require(
            response == {"kind": "negative-observed", "nonce": nonce, "role": role},
            "negative-capability-ack",
        )


def refuse_wrong_capabilities(
    channels: tuple[socket.socket, socket.socket],
    sender: int | None,
    proc_fd: int,
    nonce: str,
    end: float,
) -> list[dict[str, object]]:
    """Refuse actual wrong-capability frames, close all rights, then acknowledge exact refusal."""
    ready, go = channels
    result: list[dict[str, object]] = []
    for role, code in (("netns", "namespace-kind"), ("parent-pidns", "sender-namespace-join")):
        value, rights, peer = receive(ready, end)
        first: BaseException | None = None
        refused = False
        try:
            require(
                peer == sender
                and set(value) == {"kind", "nonce", "role", "namespace", "pidfd"}
                and value["kind"] == "namespace-negative"
                and value["nonce"] == nonce
                and value["role"] == role,
                "negative-capability-frame",
            )
            try:
                _ = identity_mapping(value, rights, peer, proc_fd)
            except ProbeError as error:
                refused = str(error) == code
            require(refused, "wrong-capability-accepted")
        except BaseException as error:  # noqa: BLE001 -- Close received capabilities on refusal.
            first = error
        finally:
            close_error, closed = retire_rights(rights)
            first = first or close_error
        if first is not None:
            raise first
        require(closed and len(rights) == PIDFD_CAPABILITIES, "wrong-capability-rights-close")
        result.append({"role": role, "refused": refused, "allRightsClosed": closed})
        require(time.monotonic() < end, "negative-ack-deadline")
        packet(go, {"kind": "negative-observed", "nonce": nonce, "role": role})
    return result


def leaf(  # noqa: C901, PLR0912, PLR0915 -- One closed inert catalog.
    arguments: Sequence[str],
) -> int:
    """Run only one inert behavior, handing original identities to PID1 before GO."""
    require(len(arguments) == LEAF_ARGUMENTS, "leaf-arguments")
    case, event_text, command_text, end_text = arguments
    require(case in (*CASES, "cleanup"), "leaf-case")
    end = float(end_text)
    facts = kernel_status()
    require(
        os.getpid() != 1
        and os.geteuid() == 0
        and math.isfinite(end)
        and cast("int", facts["namespaceDepth"]) > 0
        and time.monotonic() < end,
        "leaf-scope",
    )
    events = socket.socket(fileno=int(event_text))
    commands = socket.socket(fileno=int(command_text))
    first: BaseException | None = None
    received: list[int] = []
    status = 1
    try:
        peer_pid, peer_uid, peer_gid = struct.unpack(
            "3i", commands.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i"))
        )
        require(peer_pid == 1 and peer_uid == 0 and peer_gid == 0, "leaf-pid1-peer")
        commands.setsockopt(socket.SOL_SOCKET, socket.SO_PASSCRED, 1)
        if case in {"ignore-term", "cleanup"}:
            _ = signal.signal(signal.SIGTERM, signal.SIG_IGN)
        facts = kernel_status()
        identity_packet(events, {"kind": "leaf", "role": case, **facts})
        go, rights, sender = receive(commands, end)
        received.extend(rights)
        require(sender == 1 and not rights and go == {"kind": "leaf-go"}, "leaf-go")
        if case in {"completed-nonzero", "exit-trap-new-child"}:
            status = EXIT_FAILURE
        elif case == "terminal-leader-live-descendant":
            child = os.fork()
            if child:
                status = 0
            else:
                _ = os.setsid()
                identity_packet(events, {"kind": "orphan", "role": case, **kernel_status()})
                go, rights, sender = receive(commands, end)
                received.extend(rights)
                require(sender == 1 and not rights and go == {"kind": "leaf-go"}, "orphan-go")
                while time.monotonic() < end:
                    time.sleep(0.02)
        elif case in {"natural-zero", "helper-or-close-failure", "host-proc-netns-role"}:
            status = 0
        else:
            if case == "eof-with-live-descendant":
                os.close(1)
                os.close(2)
                packet(events, {"kind": "eof-issued", "role": case, **facts})
            while time.monotonic() < end:
                time.sleep(0.02)
    except BaseException as error:  # noqa: BLE001 -- Both inherited channel owners settle.
        first = error
    finally:
        first = remember(
            (*(lambda fd=fd: os.close(fd) for fd in received), events.close, commands.close), first
        )
    if first is not None:
        raise first
    return status


def exited(pid: int) -> bool:
    """Observe the direct launcher without consuming its reserved PID."""
    while True:
        try:
            return os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT) is not None
        except InterruptedError:
            continue


def reap_all(
    end: float,
    retire: Callable[[], None],
    observed: Callable[[int, int], None] | None = None,
) -> tuple[list[dict[str, int]], bool]:
    """PID1 consumes every adopted child; a late kernel reap stays negative, never detached."""
    values: list[dict[str, int]] = []
    overflow = False
    while True:
        try:
            pid, status = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            require(not overflow, "reap-budget")
            return values, True
        except InterruptedError:
            continue
        if not pid:
            # Repeat KILL to cover a last fork concurrent with an earlier namespace signal.
            retire()
            if time.monotonic() < end:
                time.sleep(0.01)
                continue
            # Known children are still consumed even when observed late. Kernel wait latency
            # has no hard wall bound; the caller's final fence must retain the late refusal.
            try:
                pid, status = os.waitpid(-1, 0)
            except (ChildProcessError, InterruptedError):
                continue
        # Disarm an exact consuming-wait owner before any later record/capacity validation.
        if observed is not None:
            observed(pid, status)
        if len(values) < MAX_EVENTS:
            values.append({"pid": pid, "status": os.waitstatus_to_exitcode(status)})
        else:
            overflow = True


def init(arguments: Sequence[str]) -> int:  # noqa: C901, PLR0912, PLR0915 -- One test realm owner.
    """Own a fixed PID namespace through original caps, children, pipes and ECHILD."""
    require(len(arguments) == INIT_ARGUMENTS, "init-arguments")
    index, variant, nonce, ready_text, go_text, original_text, proc_text, end_text = arguments
    require(index.isdecimal() and 0 <= int(index) < len(CASES), "case-index")
    case = CASES[int(index)]
    require(re.fullmatch(r"[a-f0-9]{32}", nonce), "nonce")
    require(variant in {"ordinary", "expired", "eof", "signal", "truncated"}, "variant")
    ready_fd, go_fd = int(ready_text), int(go_text)
    original_fd, proc_fd = int(original_text), int(proc_text)
    inherited = [ready_fd, go_fd, original_fd, proc_fd]
    require(len(set(inherited)) == len(inherited), "inherited-alias")
    ready: socket.socket | None = None
    go: socket.socket | None = None
    caps: list[Callable[[], None]] = []
    final_end = float(end_text)
    namespace: int | None = None
    pidfd: int | None = None
    first: BaseException | None = None
    child: subprocess.Popen[bytes] | None = None
    realm_empty = False
    events: list[dict[str, object]] = []
    peers: list[int] = []
    channels: list[socket.socket] = []
    output_closed = error_closed = False
    result: dict[str, object] = {"case": case, "passed": False, "workAdmitted": False}
    signal_status = 0

    def signalled(signum: int, _frame: object) -> None:
        nonlocal signal_status
        signal_status = signal_status or (130 if signum == signal.SIGINT else 143)

    def guard() -> None:
        require(namespace is not None, "namespace-admitted")
        admitted_namespace = cast("int", namespace)
        observed_proc, original_proc = Path("/proc").stat(), os.fstat(proc_fd)
        require(
            sys.platform == "linux"
            and os.getpid() == 1
            and os.geteuid() == 0
            and os.fstat(admitted_namespace).st_ino != os.fstat(original_fd).st_ino
            and (observed_proc.st_dev, observed_proc.st_ino)
            == (original_proc.st_dev, original_proc.st_ino),
            "exclusive-pid1",
        )

    def signal_realm(number: signal.Signals) -> None:
        guard()
        with contextlib.suppress(ProcessLookupError):
            os.kill(-1, number)

    def observed_reap(pid: int, raw_status: int) -> None:
        if child is not None and pid == child.pid:
            child.returncode = os.waitstatus_to_exitcode(raw_status)
            result["launcherStatus"] = child.returncode
            result["launcherConsumedOnce"] = True

    old = {n: signal.getsignal(n) for n in (signal.SIGINT, signal.SIGTERM, signal.SIGCHLD)}
    try:
        require(time.monotonic() < final_end, "init-admission")
        ready = socket.socket(fileno=ready_fd)
        inherited.remove(ready_fd)
        go = socket.socket(fileno=go_fd)
        inherited.remove(go_fd)
        caps.append(go.close)
        namespace = os.open("/proc/self/ns/pid", os.O_RDONLY | os.O_CLOEXEC)
        caps.append(lambda: os.close(namespace))
        for n in (signal.SIGINT, signal.SIGTERM):
            _ = signal.signal(n, signalled)
        _ = signal.signal(signal.SIGCHLD, signal.SIG_DFL)
        guard()
        facts = kernel_status()
        if hasattr(os, "pidfd_open"):
            pidfd = os.pidfd_open(1)
            caps.append(lambda: os.close(pidfd))
        packet(
            ready,
            {
                "kind": "ready",
                "nonce": nonce,
                "hostPid": facts["hostPid"],
                "pid": 1,
                "namespace": os.fstat(namespace).st_ino,
                "pidfd": pidfd is not None,
                "proc": [os.fstat(proc_fd).st_dev, os.fstat(proc_fd).st_ino],
            },
            (namespace,) if pidfd is None else (namespace, pidfd),
        )
        if variant == "signal":
            os.kill(1, signal.SIGINT)
        command, received_fds, _sender = receive(go, final_end)
        peers.extend(received_fds)
        require(not received_fds and frozenset(command) == GO_KEYS, "go-shape")
        require(command["kind"] == "go" and command["nonce"] == nonce, "go-binding")
        body_end = cast("float", command["bodyEnd"])
        require(type(body_end) in (int, float) and math.isfinite(body_end), "body-clock")
        require(command["finalEnd"] == final_end, "final-clock")
        require(not signal_status and time.monotonic() < body_end <= final_end, "pre-go-refusal")
        if variant == "truncated":
            require(case == "helper-or-close-failure", "truncated-role")
            held = (namespace,) if pidfd is None else (namespace, pidfd)
            oversized = b"X" * (MAX_FRAME + 1)
            require(
                ready.sendmsg(
                    [oversized],
                    [(socket.SOL_SOCKET, socket.SCM_RIGHTS, struct.pack(f"{len(held)}i", *held))],
                )
                == len(oversized),
                "truncated-send",
            )
            abort, abort_rights, _sender = receive(go, final_end)
            peers.extend(abort_rights)
            require(
                not abort_rights and abort == {"kind": "abort", "nonce": nonce}, "expected-abort"
            )
            result["protocolRefusalObserved"] = True
            message = "expected-truncated-refusal"
            raise ProbeError(message)  # noqa: TRY301 -- Expected refusal before work.
        if case == "helper-or-close-failure":
            require(pidfd is not None, "negative-self-pidfd")
            wrong_capabilities((ready, go), original_fd, cast("int", pidfd), nonce, body_end)
            result["wrongCapabilityAcknowledgements"] = True
        event_parent, event_child = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        channels.extend((event_parent, event_child))
        event_parent.setsockopt(socket.SOL_SOCKET, socket.SO_PASSCRED, 1)
        command_parent, command_child = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        channels.extend((command_parent, command_child))
        event_write, command_read = event_child.fileno(), command_child.fileno()
        argv = [
            BASH,
            "-c",
            WORKER,
            "probe",
            case,
            str(SELF),
            str(event_write),
            str(command_read),
            str(body_end),
        ]
        if case == "eof-with-live-descendant":
            argv = [
                PYTHON,
                "-I",
                "-B",
                str(SELF),
                "leaf",
                case,
                str(event_write),
                str(command_read),
                str(body_end),
            ]
        require(not signal_status and time.monotonic() < body_end, "work-admission")
        child = subprocess.Popen(  # noqa: S603 -- Only immutable fixture roles are used.
            argv,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=ENV,
            process_group=0,
            pass_fds=(event_write, command_read),
        )
        result["workAdmitted"] = True
        event_child.close()
        command_child.close()
        require(child.stdout is not None and child.stderr is not None, "launcher-pipes")
        stdout, stderr = cast("BinaryIO", child.stdout), cast("BinaryIO", child.stderr)
        with selectors.DefaultSelector() as selector:
            for fd, label in (
                (event_parent.fileno(), "event"),
                (stdout.fileno(), "stdout"),
                (stderr.fileno(), "stderr"),
            ):
                os.set_blocking(fd, False)
                _ = selector.register(fd, selectors.EVENT_READ, label)
            counts = {"stdout": 0, "stderr": 0}
            released = False
            leaf_pidfd: int | None = None
            orphan_pidfd: int | None = None
            leaf_sender: int | None = None
            admitted: set[tuple[str, str]] = set()
            meaningful = False
            eof_live = False
            while time.monotonic() < body_end:
                require(not signal_status, "interrupted")
                for key, _mask in selector.select(0.02):
                    label = cast("str", key.data)
                    if label == "event":
                        event, rights, sender = receive(event_parent, body_end, allow_eof=True)
                        if event == {"kind": "channel-eof"}:
                            _ = selector.unregister(key.fd)
                            continue
                        peers.extend(rights)
                        kind, role = event.get("kind"), event.get("role")
                        require(
                            kind in {"leaf", "orphan", "eof-issued"} and role in {case, "cleanup"},
                            "event-role",
                        )
                        if kind in {"leaf", "orphan"}:
                            require(
                                (cast("str", kind), cast("str", role)) not in admitted,
                                "duplicate-ready",
                            )
                            actual, original = os.fstat(rights[0]), os.fstat(namespace)
                            require(
                                (actual.st_dev, actual.st_ino)
                                == (original.st_dev, original.st_ino),
                                "namespace-receiver",
                            )
                            mapping = identity_mapping(event, rights, sender, proc_fd)
                            event["mapping"] = mapping
                            admitted.add((cast("str", kind), cast("str", role)))
                            if kind == "orphan":
                                require(case == "terminal-leader-live-descendant", "orphan-role")
                                orphan_pidfd = (
                                    rights[1] if len(rights) == PIDFD_CAPABILITIES else None
                                )
                            elif role == "cleanup":
                                require(case == "exit-trap-new-child", "cleanup-role")
                            else:
                                require(not released, "initial-ready-once")
                                leaf_sender = sender
                                leaf_pidfd = (
                                    rights[1] if len(rights) == PIDFD_CAPABILITIES else None
                                )
                                result["differentPgid"] = event["pgid"] != child.pid
                                if case in {
                                    "default-timeout-pgid",
                                    "terminal-leader-live-descendant",
                                    "eof-with-live-descendant",
                                }:
                                    require(leaf_pidfd is not None, "pidfd-observer-required")
                                released = True
                            require(
                                time.monotonic() < body_end and not signal_status,
                                "leaf-go-admission",
                            )
                            packet(command_parent, {"kind": "leaf-go"})
                        else:
                            require(
                                case == "eof-with-live-descendant"
                                and not rights
                                and sender == leaf_sender,
                                "eof-peer",
                            )
                        events.append(event)
                        require(len(events) <= MAX_EVENTS, "event-count")
                        continue
                    try:
                        data = os.read(key.fd, 4096)
                    except InterruptedError:
                        continue
                    if not data:
                        _ = selector.unregister(key.fd)
                        if label == "stdout":
                            output_closed = True
                        elif label == "stderr":
                            error_closed = True
                        continue
                    counts[label] += len(data)
                    require(counts[label] <= MAX_STREAM, "stream-budget")
                terminal = exited(child.pid)
                if case == "eof-with-live-descendant":
                    eof_live = output_closed and error_closed and not terminal
                    meaningful = eof_live
                elif case == "terminal-leader-live-descendant":
                    meaningful = (
                        terminal
                        and orphan_pidfd is not None
                        and not select.select([orphan_pidfd], [], [], 0)[0]
                    )
                    result["orphanLiveAfterLeaderTerminal"] = meaningful
                elif case == "exit-trap-new-child":
                    meaningful = any(e.get("role") == "cleanup" for e in events)
                elif case in {
                    "natural-zero",
                    "completed-nonzero",
                    "helper-or-close-failure",
                    "host-proc-netns-role",
                }:
                    meaningful = terminal
                else:
                    meaningful = released
                if meaningful:
                    break
            require(meaningful, "case-observation")
            result["eofWhileLeaderLive"] = eof_live
            if case == "host-proc-netns-role":
                host_pid = number(facts["hostPid"])
                fd = os.open(f"/proc/{host_pid}/ns/net", os.O_RDONLY | os.O_CLOEXEC)
                try:
                    before = os.fstat(fd)
                    alias = Path(f"/proc/self/fd/{fd}").stat()
                    result["hostProcNetnsFd"] = (before.st_dev, before.st_ino) == (
                        alias.st_dev,
                        alias.st_ino,
                    )
                finally:
                    os.close(fd)
            # This signals only the reserved launcher group; distinct timeout groups may live.
            with contextlib.suppress(ProcessLookupError):
                os.killpg(child.pid, signal.SIGTERM)
            grace_end = min(final_end, time.monotonic() + TERM_SECONDS)
            while time.monotonic() < grace_end and not exited(child.pid):
                time.sleep(0.01)
            with contextlib.suppress(ProcessLookupError):
                os.killpg(child.pid, signal.SIGKILL)
            result["launcherGroupSignalledBeforeReap"] = True
            if leaf_pidfd is not None:
                result["originalLeafAliveAfterGroupSignal"] = not select.select(
                    [leaf_pidfd], [], [], 0
                )[0]
                require(
                    case != "default-timeout-pgid" or result["originalLeafAliveAfterGroupSignal"],
                    "group-only-insufficient",
                )
            # Signal every namespace member, including a newly spawned EXIT-trap timeout.
            signal_realm(signal.SIGTERM)
            time.sleep(min(0.1, max(0, final_end - time.monotonic())))
            signal_realm(signal.SIGKILL)
            reaps, empty = reap_all(final_end, lambda: signal_realm(signal.SIGKILL), observed_reap)
            realm_empty = empty
            result["reaps"], result["echild"] = reaps, empty
            direct = [v for v in reaps if v["pid"] == child.pid]
            require(len(direct) == 1 and empty, "explicit-reaps")
            if leaf_pidfd is not None:
                require(select.select([leaf_pidfd], [], [], 0)[0], "original-leaf-terminal")
            require(child.returncode == direct[0]["status"], "launcher-reap-owner")
            # Its owner was already disarmed by the consuming-wait callback.
            for fd, label in ((stdout.fileno(), "stdout"), (stderr.fileno(), "stderr")):
                tail = bytearray()
                while True:
                    try:
                        chunk = os.read(fd, 4096)
                    except BlockingIOError:
                        require(time.monotonic() < final_end, "drain-deadline")
                        time.sleep(0.01)
                        continue
                    if not chunk:
                        break
                    tail.extend(chunk)
                    require(counts[label] + len(tail) <= MAX_STREAM, "drain-budget")
                result[label + "Eof"] = True
            require(case != "natural-zero" or child.returncode == 0, "natural-status")
            require(
                case != "completed-nonzero" or child.returncode == EXIT_FAILURE, "negative-status"
            )
            require(case != "default-timeout-pgid" or result["differentPgid"], "actual-pgid")
            require(case != "eof-with-live-descendant" or eof_live, "eof-proof")
            positive = {
                "natural-zero",
                "host-proc-netns-role",
                "helper-or-close-failure",
                "terminal-leader-live-descendant",
            }
            require(case not in positive or child.returncode == 0, "positive-launcher-status")
            require(case in positive or child.returncode != 0, "negative-launcher-status")
            result["expectedWorkStatusObserved"] = True
            if case == "helper-or-close-failure":
                body = ProbeError("fixed-body")
                read_fd, write_fd = os.pipe()
                os.close(write_fd)

                def close_fault() -> None:
                    os.close(read_fd)
                    message = "fixed-after-real-close"
                    raise OSError(message)  # noqa: TRY301 -- Intentional paired close fault.

                first_close = remember((close_fault,), body)
                result["primaryPreserved"] = first_close is body
                try:
                    _ = os.fstat(read_fd)
                except OSError as error:
                    result["actualPeerClosed"] = error.errno == errno.EBADF
                require(result["primaryPreserved"] and result["actualPeerClosed"], "first-cause")
            result["passed"] = True
    except BaseException as error:  # noqa: BLE001 -- Retain first cause through all realm cleanup.
        first = error
        result["failure"] = "expected-pre-go" if case == "late-input-or-signal" else "refusal"
        expected_pre_go = "frame-eof-or-truncation" if variant == "eof" else "pre-go-refusal"
        if (
            case == "late-input-or-signal"
            and child is None
            and isinstance(error, ProbeError)
            and str(error) == expected_pre_go
        ):
            reaps, empty = reap_all(final_end, lambda: signal_realm(signal.SIGKILL), observed_reap)
            realm_empty = empty
            result.update(
                {
                    "passed": empty and not reaps,
                    "echild": empty,
                    "expectedAdmissionRefusal": True,
                }
            )
        if (
            case == "helper-or-close-failure"
            and variant == "truncated"
            and child is None
            and result.get("protocolRefusalObserved") is True
        ):
            reaps, empty = reap_all(final_end, lambda: signal_realm(signal.SIGKILL), observed_reap)
            realm_empty = empty
            result.update(
                {
                    "passed": empty and not reaps,
                    "echild": empty,
                    "expectedAdmissionRefusal": True,
                    "positiveWorkClaim": False,
                    "failure": "expected-protocol-refusal",
                }
            )
    finally:
        if child is not None and not realm_empty:
            first = remember((lambda: signal_realm(signal.SIGKILL),), first)
            try:
                reaps, empty = reap_all(
                    final_end, lambda: signal_realm(signal.SIGKILL), observed_reap
                )
                realm_empty = empty
                result["fallbackReaps"], result["fallbackEchild"] = reaps, empty
            except BaseException as error:  # noqa: BLE001 -- Pipes/caps still close.
                first = first or error
            result["passed"] = False
            result["fallback"] = True
        actions: list[Callable[[], None]] = [lambda fd=fd: os.close(fd) for fd in peers]
        actions.extend(channel.close for channel in channels)
        if child is not None:
            actions.extend(v.close for v in (child.stdout, child.stderr) if v is not None)
        cleanup = remember(actions)
        first = first or cleanup
        result["allPipesClosed"] = cleanup is None
        if cleanup is not None:
            result["passed"] = False
        result["events"] = events
        result["finalClockPassed"] = time.monotonic() < final_end
        result["passed"] = result["passed"] is True and result["finalClockPassed"] is True

        def restore(number: signal.Signals) -> None:
            _ = signal.signal(number, old[number])

        restorations = tuple(lambda n=n: restore(n) for n in old)
        restore_error = remember(restorations)
        # Expected pre-GO signal is a test refusal, never a product status0 claim.
        caps_cleanup = remember((*(lambda fd=fd: os.close(fd) for fd in inherited), *caps))
        result["capsClosed"] = caps_cleanup is None
        result["signalsRestored"] = restore_error is None
        result["passed"] = (
            result["passed"] is True
            and caps_cleanup is None
            and restore_error is None
            and (
                not signal_status
                or (
                    case == "late-input-or-signal"
                    and variant == "signal"
                    and signal_status == INTERRUPTED_STATUS
                )
            )
        )
        channel_cleanup: BaseException | None = None
        if ready is not None:
            channel_cleanup = remember(
                (
                    lambda: packet(
                        ready,
                        {"kind": "result", "nonce": nonce, "result": result},
                    ),
                    ready.close,
                )
            )
        else:
            result["passed"] = False
    return (
        0
        if (result["passed"] is True and channel_cleanup is None and time.monotonic() < final_end)
        else 1
    )


def tool_record(path: str, end: float) -> dict[str, object]:
    """Observe actual guest tool bytes and bounded version output before any topology trial."""
    require(time.monotonic() < end, "tool-admission")
    resolved = Path(path).resolve(strict=True)
    fd = os.open(resolved, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    first: BaseException | None = None
    result: dict[str, object] = {}
    try:
        information = os.fstat(fd)
        require(stat.S_ISREG(information.st_mode) and information.st_uid == 0, "tool-owner")
        digest_value = hashlib.sha256()
        total = 0
        while content := os.read(fd, 65536):
            require(time.monotonic() < end, "tool-deadline")
            total += len(content)
            require(total <= 64 * 1024 * 1024, "tool-size")
            digest_value.update(content)
        status, output, error = bounded_process.run(
            [path, "--version"],
            env=ENV,
            limits=bounded_process.Limits(
                timeout=max(0.001, min(5, end - time.monotonic())),
                stdout=MAX_STREAM,
                stderr=MAX_STREAM,
            ),
        )
        require(status == 0 and not error, "tool-version")
        require(
            stamp(os.fstat(fd)) == stamp(information)
            and stamp(resolved.stat()) == stamp(information),
            "tool-changed",
        )
        result = {
            "path": path,
            "resolved": str(resolved),
            "sha256": digest_value.hexdigest(),
            "version": output.decode().splitlines()[0],
        }
    except BaseException as error:  # noqa: BLE001 -- Preserve body through original close.
        first = error
    finally:
        first = remember((lambda: os.close(fd),), first)
    if first is not None:
        raise first
    return result


def trial_response(
    channels: tuple[socket.socket, socket.socket], nonce: str, end: float, expected: int | None
) -> tuple[dict[str, object], list[int], int | None, list[dict[str, object]]]:
    """Advance a truncated-rights trial only after its actual refusal and peer closes."""
    ready, go = channels
    facts: list[dict[str, object]] = []
    try:
        response, extra, sender = receive(ready, end, refusals=facts)
    except ProbeError as error:
        require(
            expected is not None
            and str(error) == "frame-eof-or-truncation"
            and len(facts) == 1
            and facts[0]["truncated"] is True
            and facts[0]["allRightsClosed"] is True
            and facts[0]["rightsReceived"] == expected,
            "expected-truncated-rights-refusal",
        )
        packet(go, {"kind": "abort", "nonce": nonce})
        response, extra, sender = receive(ready, end)
    return response, extra, sender, facts


def trial(  # noqa: PLR0915 -- One wrapper/capability owner.
    index: int, variant: str, nonce: str, end: float
) -> dict[str, object]:
    """Own actual unshare wrapper, received namespace caps and every pipe through final reap."""
    owners: list[Callable[[], None]] = []
    case_end = min(end, time.monotonic() + CASE_SECONDS)
    first: BaseException | None = None
    child: subprocess.Popen[bytes] | None = None
    received: list[int] = []
    result: dict[str, object] = {}
    try:
        require(time.monotonic() < case_end, "case-admission")
        ready_parent, ready_child = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        owners.extend((ready_parent.close, ready_child.close))
        go_parent, go_child = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        owners.extend((go_parent.close, go_child.close))
        ready_parent.setsockopt(socket.SOL_SOCKET, socket.SO_PASSCRED, 1)
        parent_ns = os.open("/proc/self/ns/pid", os.O_RDONLY | os.O_CLOEXEC)
        owners.append(lambda: os.close(parent_ns))
        parent_proc = os.open("/proc", os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
        owners.append(lambda: os.close(parent_proc))
        child = subprocess.Popen(  # noqa: S603 -- Fixed unshare and this exact role only.
            [
                UNSHARE,
                "--pid",
                "--fork",
                "--kill-child=KILL",
                PYTHON,
                "-I",
                "-B",
                str(SELF),
                "init",
                str(index),
                variant,
                nonce,
                str(ready_child.fileno()),
                str(go_child.fileno()),
                str(parent_ns),
                str(parent_proc),
                str(case_end),
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=ENV,
            process_group=0,
            pass_fds=(ready_child.fileno(), go_child.fileno(), parent_ns, parent_proc),
        )
        ready_child.close()
        go_child.close()
        ready, received, sender = receive(ready_parent, case_end)
        require(
            frozenset(ready) == READY_KEYS
            and ready["kind"] == "ready"
            and ready["nonce"] == nonce
            and ready["pid"] == 1,
            "ready-shape",
        )
        require(
            sender == ready["hostPid"] and len(received) == (2 if ready["pidfd"] else 1),
            "ready-capabilities",
        )
        namespace = os.fstat(received[0])
        require(
            namespace.st_ino == ready["namespace"]
            and namespace.st_ino != os.fstat(parent_ns).st_ino,
            "namespace-original",
        )
        mapping = identity_mapping(ready, received, sender, parent_proc, expect_init=True)
        require(
            ready["proc"] == [os.fstat(parent_proc).st_dev, os.fstat(parent_proc).st_ino],
            "host-proc-original",
        )
        require(time.monotonic() < case_end, "go-original-clock")
        if variant == "eof":
            go_parent.close()
        else:
            packet(
                go_parent,
                {
                    "kind": "go",
                    "nonce": nonce,
                    "bodyEnd": time.monotonic() - 1
                    if variant == "expired"
                    else case_end - REAP_SECONDS,
                    "finalEnd": case_end,
                },
            )
        wrong_facts: list[dict[str, object]] = []
        if CASES[index] == "helper-or-close-failure" and variant == "ordinary":
            wrong_facts = refuse_wrong_capabilities(
                (ready_parent, go_parent), sender, parent_proc, nonce, case_end
            )
        response, extra, result_sender, refusal_facts = trial_response(
            (ready_parent, go_parent),
            nonce,
            case_end,
            len(received) if variant == "truncated" else None,
        )
        require(result_sender == sender, "result-sender")
        received.extend(extra)
        require(
            not extra
            and set(response) == {"kind", "nonce", "result"}
            and response["kind"] == "result"
            and response["nonce"] == nonce,
            "result-shape",
        )
        require(isinstance(response["result"], dict), "result-object")
        result = cast("dict[str, object]", response["result"])
        if variant == "truncated":
            require(
                result.get("protocolRefusalObserved") is True
                and result.get("workAdmitted") is False,
                "no-work-after-truncation",
            )
            result["refusedFrameFacts"] = refusal_facts
        while time.monotonic() < case_end and not exited(child.pid):
            time.sleep(0.01)
        require(exited(child.pid), "wrapper-terminal")
        with contextlib.suppress(ProcessLookupError):
            os.killpg(child.pid, signal.SIGKILL)
        output, error = io.BytesIO(), io.BytesIO()
        status = bounded_process.pump(
            child,
            output,
            error,
            bounded_process.Limits(
                timeout=max(0.001, case_end - time.monotonic()),
                stdout=MAX_STREAM,
                stderr=MAX_STREAM,
            ),
        )
        require(status == 0, "wrapper-status")
        if len(received) == PIDFD_CAPABILITIES:
            require(
                select.select([received[1]], [], [], max(0, case_end - time.monotonic()))[0],
                "namespace-init-terminal",
            )
        result.update(
            {
                "wrapperReaped": True,
                "wrapperStatus": child.returncode,
                "wrapperStreamsEof": True,
                "namespaceOriginal": True,
                "pidfdAvailable": ready["pidfd"],
                "identityMapping": mapping,
                "wrongCapabilityRefusals": wrong_facts,
            }
        )
    except BaseException as error:  # noqa: BLE001 -- Never detach an admitted wrapper on refusal.
        first = error
    finally:
        if child is not None and child.returncode is None:
            first = remember((lambda: bounded_process.stop(child),), first)
        actions: list[Callable[[], None]] = list(owners)
        actions.extend(lambda fd=fd: os.close(fd) for fd in received)
        if child is not None:
            actions.extend(v.close for v in (child.stdout, child.stderr) if v is not None)
        first = remember(actions, first)
    if first is not None:
        raise first
    require(result.get("passed") is True and time.monotonic() < min(end, case_end), "case-failed")
    return result


def package_versions(end: float) -> dict[str, str]:
    """Observe exactly the three Debian source-package identities under the same end."""
    package_status, package_output, package_error = bounded_process.run(
        [
            "/usr/bin/dpkg-query",
            "-W",
            "-f=${binary:Package}\t${Version}\t${Architecture}\n",
            "coreutils",
            "util-linux",
            "bash",
        ],
        env=ENV,
        limits=bounded_process.Limits(
            timeout=min(5, end - time.monotonic()), stdout=MAX_STREAM, stderr=MAX_STREAM
        ),
    )
    require(package_status == 0 and not package_error, "package-observation")
    result: dict[str, str] = {}
    for line in package_output.decode().splitlines():
        name, version, architecture = line.split("\t")
        require(
            name in {"coreutils", "util-linux", "bash"}
            and name not in result
            and architecture == "amd64"
            and re.fullmatch(r"[A-Za-z0-9.+:~_-]{1,128}", version),
            "package-shape",
        )
        result[name] = version
    require(set(result) == {"coreutils", "util-linux", "bash"}, "package-set")
    return result


def debian_identity() -> dict[str, str]:
    """Observe fixed guest release/architecture without claiming package-byte authority."""
    release = platform.freedesktop_os_release()
    version = release.get("VERSION_ID", "")
    require(
        release.get("ID") == "debian"
        and re.fullmatch(r"13(?:\.[0-9]{1,3})?", version)
        and os.uname().machine == "x86_64",
        "debian-amd64-profile",
    )
    return {"id": "debian", "versionId": version, "architecture": "amd64"}


def run(arguments: Sequence[str]) -> int:
    """Execute all eleven cases only in the exact fresh marked disposable guest."""
    require(len(arguments) == GUEST_ARGUMENTS, "guest-arguments")
    nonce, seconds_text = arguments
    require(
        sys.platform == "linux" and os.geteuid() == 0 and re.fullmatch(r"[a-f0-9]{32}", nonce),
        "guest-guard",
    )
    marker = Path("/etc/simplestchat-vm-fixture")
    require(marker.read_bytes() == (nonce + "\n").encode(), "guest-marker")
    seconds = float(seconds_text)
    require(math.isfinite(seconds) and MINIMUM_WORK <= seconds <= BODY_SECONDS, "guest-budget")
    end = time.monotonic() + seconds
    records: list[dict[str, object]] = []
    final_records: list[dict[str, object]] = []
    versions: dict[str, str] = {}
    distribution: dict[str, str] = {}
    cases: list[dict[str, object]] = []
    first: BaseException | None = None
    original_signals = {n: signal.getsignal(n) for n in (signal.SIGINT, signal.SIGTERM)}
    pending_status = 0

    def interrupted(signum: int, _frame: object) -> None:
        nonlocal pending_status
        pending_status = pending_status or (130 if signum == signal.SIGINT else 143)

    def restore(number: signal.Signals) -> None:
        _ = signal.signal(number, original_signals[number])

    try:
        for n in original_signals:
            _ = signal.signal(n, interrupted)
        distribution = debian_identity()
        records = [tool_record(path, end) for path in TOOLS]
        versions = package_versions(end)
        for index, name in enumerate(CASES):
            require(not pending_status and time.monotonic() < end, "catalog-admission")
            variants = CASE_VARIANTS.get(name, ("ordinary",))
            values = [trial(index, variant, nonce, end) for variant in variants]
            cases.append({"name": name, "passed": True, "trials": values})
            require(not pending_status, "catalog-interrupted")
        final_records = [tool_record(path, end) for path in TOOLS]
        require(final_records == records, "final-tools-changed")
    except BaseException as error:  # noqa: BLE001 -- Every known signal owner restores.
        first = error
        if len(cases) < len(CASES):
            cases.append({"name": CASES[len(cases)], "passed": False, "failure": "refusal"})
    finally:
        first = remember(tuple(lambda n=n: restore(n) for n in original_signals), first)
    report: dict[str, object] = {
        "schemaVersion": 1,
        "scope": "TestOnlyMaintainerNamespaceProbe",
        "runId": nonce,
        "passed": first is None and not pending_status and time.monotonic() < end,
        "failure": None if first is None else "catalog-unproven",
        "exception": exception_kind(first),
        "outerSignalStatus": pending_status,
        "tools": records,
        "finalToolsUnchanged": final_records == records,
        "packages": versions,
        "distribution": distribution,
        "kernel": os.uname().release,
        "cases": cases,
        "actualLauncherExecuted": False,
    }
    raw = json.dumps(report, separators=(",", ":"), allow_nan=False).encode() + b"\n"
    require(len(raw) <= MAX_EVENT_BYTES * 1024, "report-budget")
    require(sys.stdout.buffer.write(raw) == len(raw), "report-output-short")
    _ = sys.stdout.buffer.flush()
    # Earlier report bytes are passive until stdio and this original end/signal fence pass.
    return pending_status or (0 if report["passed"] and time.monotonic() < end else 1)


def main() -> int:
    """Expose only fixed private test roles, with no arbitrary command or host input."""
    try:
        require(len(sys.argv) >= ROLE_ARGUMENTS, "role")
        role = sys.argv[1]
        if role == "init":
            return init(sys.argv[2:])
        if role == "leaf":
            return leaf(sys.argv[2:])
        require(role == "run", "role")
        return run(sys.argv[2:])
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
        _ = sys.stderr.write("Fixed private namespace prototype is unproven.\n")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
