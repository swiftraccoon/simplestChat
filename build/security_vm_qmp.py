"""Observe bounded lifecycle events from the controller's private QEMU Unix socket.

Only capabilities negotiation is sent. QEMU can run before it completes, so an
absent event never establishes that no reset or shutdown occurred. No raw QMP
text, guest paths, error descriptions or arbitrary event values leave this helper.
"""

from __future__ import annotations

import errno
import json
import math
import os
import socket
import stat
import threading
import time
from contextlib import suppress
from typing import TYPE_CHECKING, cast, final

from security_tools import ToolError, require

if TYPE_CHECKING:
    from pathlib import Path

MAX_BYTES = 65536
MAX_MESSAGES = 128
MAX_LINE = 8192
MAX_PATH = 100
MAX_VERSION = 65535
MAX_CAPABILITIES = 32
CONNECT_SECONDS = 10.0
POLL_SECONDS = 0.1
CLOSE_SECONDS = 1.0
CAPABILITIES = b'{"execute":"qmp_capabilities","id":"capabilities"}\r\n'
REASONS = frozenset(
    {
        "none",
        "host-error",
        "host-qmp-quit",
        "host-qmp-system-reset",
        "host-signal",
        "host-ui",
        "guest-shutdown",
        "guest-reset",
        "guest-panic",
        "subsystem-reset",
        "snapshot-load",
    }
)


def object_value(value: object) -> dict[str, object]:
    """Reject malformed protocol objects with a fixed, non-sensitive code."""
    require(isinstance(value, dict), "vm_qmp_message")
    return cast("dict[str, object]", value)


def validate_path(path: Path) -> None:
    """Confine observation to the caller's private, directly owned Unix directory."""
    require(path.is_absolute() and path == path.resolve(), "vm_qmp_path")
    require(len(os.fsencode(path)) <= MAX_PATH, "vm_qmp_path")
    info = path.parent.stat()
    require(
        stat.S_ISDIR(info.st_mode)
        and info.st_uid == os.getuid()
        and stat.S_IMODE(info.st_mode) & 0o077 == 0,
        "vm_qmp_parent",
    )


@final
class Observer:
    """Own one finite observer thread without controlling or reaping the guest."""

    def __init__(self, path: Path, deadline: float) -> None:
        """Validate the owned endpoint without creating a socket or starting a process."""
        validate_path(path)
        require(math.isfinite(deadline) and deadline > time.monotonic(), "vm_qmp_deadline")
        self.path = path
        self.deadline = deadline
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self._socket: socket.socket | None = None
        self._failure: str | None = None
        self._connected = False
        self._greeting = False
        self._negotiated = False
        self._eof = False
        self._bytes = 0
        self._messages = 0
        self._withheld = 0
        self._events: list[dict[str, object]] = []

    @property
    def failure(self) -> str | None:
        """Expose only the first fixed failure, except an incomplete cleanup overrides it."""
        with self._lock:
            return self._failure

    def _fail(self, code: str) -> None:
        with self._lock:
            if self._failure is None or code == "vm_qmp_cleanup_incomplete":
                self._failure = code

    def start(self) -> None:
        """Start once; startup failures remain available to the controller's cleanup guard."""
        require(self._thread is None and not self._stop.is_set(), "vm_qmp_started")
        self._thread = threading.Thread(target=self._run, name="owned-qmp", daemon=True)
        try:
            self._thread.start()
        except RuntimeError as error:
            self._fail("vm_qmp_start")
            code = "vm_qmp_start"
            raise ToolError(code) from error

    def _connect(self) -> socket.socket:
        connect_until = min(self.deadline, time.monotonic() + CONNECT_SECONDS)
        while not self._stop.is_set() and time.monotonic() < connect_until:
            try:
                info = self.path.lstat()
                require(
                    stat.S_ISSOCK(info.st_mode)
                    and info.st_uid == os.getuid()
                    and stat.S_IMODE(info.st_mode) & 0o077 == 0,
                    "vm_qmp_socket",
                )
                channel = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                try:
                    channel.settimeout(POLL_SECONDS)
                    channel.connect(str(self.path))
                except OSError:
                    channel.close()
                    raise
                with self._lock:
                    self._connected = True
                    self._socket = channel
            except OSError as error:
                if error.errno not in {errno.ENOENT, errno.ECONNREFUSED}:
                    raise
            else:
                return channel
            _ = self._stop.wait(POLL_SECONDS)
        code = "vm_qmp_connect_incomplete"
        raise ToolError(code)

    def _hello(self, message: dict[str, object], channel: socket.socket) -> None:
        require(set(message) == {"QMP"}, "vm_qmp_greeting")
        hello = object_value(message["QMP"])
        version = object_value(object_value(hello.get("version")).get("qemu"))
        for name in ("major", "minor", "micro"):
            value = version.get(name)
            require(type(value) is int and 0 <= value <= MAX_VERSION, "vm_qmp_greeting")
        capabilities = hello.get("capabilities")
        require(isinstance(capabilities, list), "vm_qmp_greeting")
        require(len(cast("list[object]", capabilities)) <= MAX_CAPABILITIES, "vm_qmp_greeting")
        channel.sendall(CAPABILITIES)
        self._greeting = True

    def _event(self, message: dict[str, object]) -> None:
        event = message.get("event")
        require(isinstance(event, str), "vm_qmp_event")
        if event not in {"SHUTDOWN", "RESET"}:
            self._withheld += 1
            return
        data = object_value(message.get("data"))
        reason, guest = data.get("reason"), data.get("guest")
        require(isinstance(reason, str) and reason in REASONS, "vm_qmp_reason")
        require(type(guest) is bool, "vm_qmp_guest")
        self._events.append({"event": event, "reason": reason, "guest": guest})

    def _message(self, raw: bytes, channel: socket.socket) -> None:
        self._messages += 1
        require(self._messages <= MAX_MESSAGES, "vm_qmp_message_limit")
        message = object_value(cast("object", json.loads(raw)))
        if not self._greeting:
            self._hello(message, channel)
        elif "event" in message:
            self._event(message)
        else:
            require(
                not self._negotiated
                and set(message) == {"return", "id"}
                and message["return"] == {}
                and message["id"] == "capabilities",
                "vm_qmp_negotiation",
            )
            self._negotiated = True

    def _read(self, channel: socket.socket) -> None:
        pending = b""
        while not self._stop.is_set():
            require(time.monotonic() < self.deadline, "vm_qmp_deadline")
            try:
                data = channel.recv(4096)
            except TimeoutError:
                continue
            if not data:
                require(not pending.strip() and self._negotiated, "vm_qmp_eof_incomplete")
                self._eof = True
                return
            self._bytes += len(data)
            require(self._bytes <= MAX_BYTES, "vm_qmp_byte_limit")
            pending += data
            while b"\n" in pending:
                raw, pending = pending.split(b"\n", 1)
                require(len(raw) <= MAX_LINE, "vm_qmp_line_limit")
                self._message(raw, channel)
            require(len(pending) <= MAX_LINE, "vm_qmp_line_limit")
        code = "vm_qmp_observation_incomplete"
        raise ToolError(code)

    def _run(self) -> None:
        try:
            with self._connect() as channel:
                self._read(channel)
        except ToolError as error:
            self._fail(str(error))
        except (ValueError, RecursionError):
            self._fail("vm_qmp_json")
        except OSError:
            self._fail("vm_qmp_io")
        except Exception:  # noqa: BLE001 -- never publish unexpected library exception text.
            self._fail("vm_qmp_observer")

    def close(self) -> None:
        """Bound final EOF observation after child cleanup; never raise ordinary I/O errors."""
        thread = self._thread
        if thread is None or thread.ident is None:
            return
        thread.join(CLOSE_SECONDS)
        if thread.is_alive():
            self._fail("vm_qmp_observation_incomplete")
            self._stop.set()
            channel = self._socket
            if channel is not None:
                with suppress(OSError):
                    channel.shutdown(socket.SHUT_RDWR)
            thread.join(CLOSE_SECONDS)
        if thread.is_alive():
            self._fail("vm_qmp_cleanup_incomplete")

    def diagnostics(self) -> dict[str, object]:
        """Return fixed lifecycle vocabulary, counts and explicit observation limitations."""
        with self._lock:
            return {
                "connected": self._connected,
                "greetingReceived": self._greeting,
                "capabilitiesNegotiated": self._negotiated,
                "eofObserved": self._eof,
                "bytes": self._bytes,
                "messages": self._messages,
                "withheldEvents": self._withheld,
                "events": [dict(event) for event in self._events],
                "failure": self._failure,
                "preNegotiationEventsObservable": False,
                "absenceProvesNoEvent": False,
            }
