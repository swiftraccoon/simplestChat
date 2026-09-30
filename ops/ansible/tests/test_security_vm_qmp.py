"""Exercise only private local Unix sockets and bounded, inert QMP-shaped messages."""

from __future__ import annotations

import json
import os
import socket
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "build"))

import security_vm_qmp as qmp
from security_tools import ToolError

PRIVATE = "private-fixture-never-published"
GREETING: dict[str, object] = {
    "QMP": {
        "version": {"qemu": {"major": 8, "minor": 2, "micro": 2}, "package": PRIVATE},
        "capabilities": [],
    },
}
ACK: dict[str, object] = {"return": {}, "id": "capabilities"}


def encoded(value: object) -> bytes:
    """Represent one inert protocol frame, including the documented CRLF terminator."""
    return json.dumps(value).encode() + b"\r\n"


def event(reason: str = "guest-reset", *, guest: object = True) -> dict[str, object]:
    """Build a fixed event fixture, allowing explicit malformed values in negative tests."""
    return {"event": "SHUTDOWN", "data": {"guest": guest, "reason": reason}}


class ObserverTests(unittest.TestCase):
    """Observe exact owned endpoints without starting QEMU or sending control commands."""

    def transcript(self, payload: bytes, *, hello: bool = True) -> dict[str, object]:
        """Feed a finite private stream, assert the sole command, and collect safe metadata."""
        with (
            tempfile.TemporaryDirectory(prefix="qmp-", dir="/tmp") as temporary,
            socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener,
        ):
            path = Path(temporary).resolve() / "q.sock"
            listener.bind(str(path))
            path.chmod(0o600)
            listener.listen(1)
            listener.settimeout(2)
            observer = qmp.Observer(path, time.monotonic() + 5)
            observer.start()
            try:
                channel = listener.accept()[0]
                with channel:
                    channel.settimeout(2)
                    if hello:
                        channel.sendall(encoded(GREETING))
                        command = b""
                        while not command.endswith(b"\n"):
                            data = channel.recv(1024)
                            self.assertTrue(data)
                            command += data
                        self.assertEqual(command, qmp.CAPABILITIES)
                    channel.sendall(payload)
            finally:
                observer.close()
            self.assertTrue(path.is_socket(), "Observer does not unlink a controller-owned path")
            result = observer.diagnostics()
            self.assertNotIn(PRIVATE, json.dumps(result))
            return result

    def test_success_has_only_fixed_events_and_explicit_observation_gaps(self) -> None:
        """Raw package/event fields are withheld; lifecycle semantics retain no arbitrary text."""
        payload = (
            encoded(ACK)
            + encoded({"event": PRIVATE, "data": {"private": PRIVATE}})
            + encoded(
                {"event": "RESET", "data": {"guest": False, "reason": "host-qmp-system-reset"}}
            )
            + encoded(event())
        )
        result = self.transcript(payload)
        self.assertIsNone(result["failure"])
        self.assertTrue(result["capabilitiesNegotiated"])
        self.assertTrue(result["eofObserved"])
        self.assertEqual(result["withheldEvents"], 1)
        self.assertEqual(
            result["events"],
            [
                {"event": "RESET", "guest": False, "reason": "host-qmp-system-reset"},
                {"event": "SHUTDOWN", "guest": True, "reason": "guest-reset"},
            ],
        )
        self.assertFalse(result["absenceProvesNoEvent"])
        self.assertFalse(result["preNegotiationEventsObservable"])

    def test_event_can_interleave_response_but_acknowledgement_remains_required(self) -> None:
        """An observed event does not turn missing negotiation into successful evidence."""
        result = self.transcript(encoded(event()) + encoded(ACK))
        self.assertIsNone(result["failure"])
        result = self.transcript(encoded(event()))
        self.assertEqual(result["failure"], "vm_qmp_eof_incomplete")
        self.assertFalse(result["capabilitiesNegotiated"])

    def test_negotiated_empty_stream_does_not_prove_absence(self) -> None:
        """Successful observation with no lifecycle event retains the explicit gap."""
        result = self.transcript(encoded(ACK))
        self.assertIsNone(result["failure"])
        self.assertEqual(result["events"], [])
        self.assertFalse(result["absenceProvesNoEvent"])

    def test_protocol_errors_never_publish_raw_error_or_reason(self) -> None:
        """Only the exact capability reply and fixed typed lifecycle values are accepted."""
        cases: tuple[tuple[object, str], ...] = (
            (
                {"error": {"class": PRIVATE, "desc": PRIVATE}, "id": "capabilities"},
                "vm_qmp_negotiation",
            ),
            ({"return": {}, "id": PRIVATE}, "vm_qmp_negotiation"),
            (event(PRIVATE), "vm_qmp_reason"),
            (event(guest=1), "vm_qmp_guest"),
        )
        for value, code in cases:
            with self.subTest(code=code, value=value):
                result = self.transcript(encoded(value))
                self.assertEqual(result["failure"], code)

    def test_malformed_greeting_json_and_partial_eof_fail(self) -> None:
        """Malformed and incomplete streams cannot yield successful observation metadata."""
        for payload, hello, code in (
            (encoded({"QMP": None}), False, "vm_qmp_message"),
            (b"not-json\r\n", False, "vm_qmp_json"),
            (encoded(ACK) + b'{"event":', True, "vm_qmp_eof_incomplete"),
            (b"", True, "vm_qmp_eof_incomplete"),
        ):
            with self.subTest(code=code, payload=payload):
                self.assertEqual(self.transcript(payload, hello=hello)["failure"], code)

    def test_line_byte_and_message_limits_fail_instead_of_truncating(self) -> None:
        """Every declared bound is enforced on actual incoming socket bytes."""
        cases = (
            ("MAX_LINE", 256, b"x" * 257, "vm_qmp_line_limit"),
            ("MAX_BYTES", 256, b"x" * 300, "vm_qmp_byte_limit"),
            ("MAX_MESSAGES", 2, encoded(ACK) + encoded(event()), "vm_qmp_message_limit"),
        )
        for name, limit, payload, code in cases:
            with self.subTest(name=name), patch.object(qmp, name, limit):
                self.assertEqual(self.transcript(payload)["failure"], code)

    def test_unowned_or_non_socket_endpoint_fails_without_connection(self) -> None:
        """A private directory alone cannot authorize a regular file or public socket."""
        with tempfile.TemporaryDirectory(prefix="qmp-", dir="/tmp") as temporary:
            path = Path(temporary).resolve() / "q.sock"
            _ = path.write_text("inert")
            observer = qmp.Observer(path, time.monotonic() + 2)
            observer.start()
            observer.close()
            self.assertEqual(observer.failure, "vm_qmp_socket")
            path.unlink()
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
                listener.bind(str(path))
                path.chmod(0o666)
                observer = qmp.Observer(path, time.monotonic() + 2)
                observer.start()
                observer.close()
                self.assertEqual(observer.failure, "vm_qmp_socket")

    def test_path_parent_and_deadline_validation_precede_thread_start(self) -> None:
        """Reject symlinks, public parents, long Unix paths and expired/nonfinite limits."""
        with tempfile.TemporaryDirectory(prefix="qmp-", dir="/tmp") as temporary:
            root = Path(temporary).resolve()
            path = root / "q.sock"
            for value in (time.monotonic() - 1, float("inf"), float("nan")):
                with self.assertRaisesRegex(ToolError, "vm_qmp_deadline"):
                    _ = qmp.Observer(path, value)
            with self.assertRaisesRegex(ToolError, "vm_qmp_path"):
                _ = qmp.Observer(root / ("x" * 100), time.monotonic() + 5)
            linked = root / "linked"
            linked.symlink_to(root, target_is_directory=True)
            with self.assertRaisesRegex(ToolError, "vm_qmp_path"):
                _ = qmp.Observer(linked / "q.sock", time.monotonic() + 5)
            root.chmod(0o755)
            try:
                with self.assertRaisesRegex(ToolError, "vm_qmp_parent"):
                    _ = qmp.Observer(path, time.monotonic() + 5)
            finally:
                root.chmod(0o700)

    def test_connect_and_shared_deadline_are_finite(self) -> None:
        """An absent owned server fails within its connect limit rather than hanging."""
        with (
            tempfile.TemporaryDirectory(prefix="qmp-", dir="/tmp") as temporary,
            patch.object(qmp, "CONNECT_SECONDS", 0.03),
            patch.object(qmp, "POLL_SECONDS", 0.01),
        ):
            observer = qmp.Observer(Path(temporary).resolve() / "q.sock", time.monotonic() + 1)
            observer.start()
            observer.close()
            self.assertEqual(observer.failure, "vm_qmp_connect_incomplete")

    def test_close_cancels_unfinished_connect_without_waiting_full_deadline(self) -> None:
        """Exact-child cleanup is followed by bounded observer cancellation and fixed failure."""
        with (
            tempfile.TemporaryDirectory(prefix="qmp-", dir="/tmp") as temporary,
            patch.object(qmp, "CLOSE_SECONDS", 0.05),
        ):
            observer = qmp.Observer(Path(temporary).resolve() / "q.sock", time.monotonic() + 30)
            observer.start()
            before = time.monotonic()
            observer.close()
            self.assertLess(time.monotonic() - before, 1)
            self.assertEqual(observer.failure, "vm_qmp_observation_incomplete")
            observer.close()

    def test_open_connection_obeys_shared_deadline_and_explicit_close(self) -> None:
        """A silent negotiated socket cannot outlive the controller or hide cancelled reads."""
        for deadline_seconds, close_seconds, expected in (
            (0.2, 1.0, "vm_qmp_deadline"),
            (5.0, 0.05, "vm_qmp_observation_incomplete"),
        ):
            with (
                self.subTest(expected=expected),
                tempfile.TemporaryDirectory(prefix="qmp-", dir="/tmp") as temporary,
                socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener,
                patch.object(qmp, "CLOSE_SECONDS", close_seconds),
            ):
                path = Path(temporary).resolve() / "q.sock"
                listener.bind(str(path))
                path.chmod(0o600)
                listener.listen(1)
                listener.settimeout(2)
                observer = qmp.Observer(path, time.monotonic() + deadline_seconds)
                observer.start()
                try:
                    with listener.accept()[0] as channel:
                        channel.settimeout(2)
                        channel.sendall(encoded(GREETING))
                        self.assertEqual(channel.recv(1024), qmp.CAPABILITIES)
                        channel.sendall(encoded(ACK))
                        observer.close()
                        self.assertEqual(observer.failure, expected)
                finally:
                    observer.close()

    def test_owner_check_is_exact(self) -> None:
        """A different owner is refused even for an otherwise private directory."""
        with tempfile.TemporaryDirectory(prefix="qmp-", dir="/tmp") as temporary:
            path = Path(temporary).resolve() / "q.sock"
            with (
                patch.object(os, "getuid", return_value=os.getuid() + 1),
                self.assertRaisesRegex(ToolError, "vm_qmp_parent"),
            ):
                _ = qmp.Observer(path, time.monotonic() + 5)


if __name__ == "__main__":
    _ = unittest.main()
