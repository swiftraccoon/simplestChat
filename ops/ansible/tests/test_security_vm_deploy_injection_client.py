"""Check actual barrier client framing/receipts with local owned files and socketpairs.

No helper entry, mounts, Docker, VM, OS signal or process-global mask changes.
Only the fixed root identity observation and already-connected socket boundary
are inert; Original receipt reads, bytes, framing, EOF and closes remain real.
"""

from __future__ import annotations

import concurrent.futures
import hashlib
import io
import json
import os
import socket
import sys
import tempfile
import time
import unittest
from contextlib import ExitStack
from pathlib import Path
from typing import TYPE_CHECKING, cast, final
from unittest.mock import patch

from test_support import ROOT, obj

# isort: split
import security_vm_deploy_injection as injection
import security_vm_ops_guest as ops_guest
from release_json import json_value, object_value

if TYPE_CHECKING:
    from collections.abc import Mapping

    from release_json import JsonObject

NONCE = "a" * 32
IDENTIFIER = "b" * 64
IMAGE = "sha256:" + "c" * 64
OUT = b'{"id":"INERT-CANDIDATE","state":{"Running":true}}\n'
ERR = b"INERT fixed Docker warning\n"
FSTAT = os.fstat
LSTAT = Path.lstat
CHECK = injection.check
SOCKET = socket.socket


def write(path: Path, data: bytes, mode: int = 0o600) -> None:
    """Create privately, then apply the final selected mode after complete bytes and flush."""
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "wb") as stream:
        if stream.write(data) != len(data):
            message = "inert client fixture short write"
            raise OSError(message)
        stream.flush()
        os.fchmod(stream.fileno(), mode)


def mapped(value: os.stat_result) -> os.stat_result:
    """Map only UID/GID, retaining real modes, identity, counts and nanosecond timestamps."""
    fields = list(value)
    fields[4] = fields[5] = 0
    return os.stat_result(
        fields,
        {
            "st_atime_ns": value.st_atime_ns,
            "st_mtime_ns": value.st_mtime_ns,
            "st_ctime_ns": value.st_ctime_ns,
        },
    )


def receipt_value() -> dict[str, object]:
    """Supply the fixed parent receipt; no real daemon effect is claimed by this fixture."""
    return {
        "schemaVersion": 1,
        "scope": "HostDeployInspectionBarrier",
        "nonce": NONCE,
        "identifier": IDENTIFIER,
        "image": IMAGE,
        "responseRawExit": 0,
        "stdoutBytes": len(OUT),
        "stdoutSha256": hashlib.sha256(OUT).hexdigest(),
        "stderrBytes": len(ERR),
        "stderrSha256": hashlib.sha256(ERR).hexdigest(),
        "runningBefore": True,
        "runningAfter": False,
        "stopRawExit": 0,
        "finishedChanged": True,
    }


@final
class Context:
    """Lower fixture context; actual Original owns every alias and receipt descriptor."""

    def __init__(self, root: Path) -> None:
        """Retain one finite original test interval, without helper entry or host paths."""
        self.end = time.monotonic() + 2
        self.nonce, self.image, self.root = NONCE, IMAGE, root
        self.originals: list[injection.Original] = []
        self.descriptors: list[int] = []
        self.signal = False
        self.signal_error = InterruptedError("INERT_LATCHED_SIGNAL")
        self.trace: list[str] = []

    def open(
        self, path: Path, maximum: int, mode: int, *, empty: bool = False
    ) -> injection.Original:
        """Keep actual same-file admission, reads and final close in the tested path."""
        original = injection.Original(path, maximum, mode, empty=empty)
        self.originals.append(original)
        self.descriptors.append(original.number())
        return original

    def real(self) -> injection.Original:
        """Retain an inert executable leaf; never execute it."""
        return self.open(self.root / "original", 128, 0o755)

    def check(self) -> None:
        """Use the real supplied deadline and already-admitted Original witnesses."""
        injection.check(self.end)
        for original in self.originals:
            original.check()

    def close(self) -> None:
        """Attempt every actual original even after a prior client refusal."""
        first = ops_guest.settle(tuple(original.close for original in reversed(self.originals)))
        if first is not None:
            raise first


@final
class Output:
    """Bounded captured original bytes with a named lower write/flush observation fault."""

    def __init__(self, context: Context, role: str, fault: str) -> None:
        """Keep the same context and exact exception identity through later peer closure."""
        self.context, self.role, self.fault = context, role, fault
        self.buffer = io.BytesIO()
        self.error = OSError("INERT_OUTPUT_FAILURE")

    def write(self, data: bytes) -> int:
        """Observe actual complete or partial writes without a real terminal."""
        self.context.trace.append(self.role + "-write")
        if self.role == "stdout" and self.fault in {"write", "write-close"}:
            raise self.error
        if self.role == "stdout" and self.fault == "short-write":
            return self.buffer.write(data[:-1])
        return self.buffer.write(data)

    def flush(self) -> None:
        """Observe the tested post-write boundary without changing OS signal handlers."""
        self.context.trace.append(self.role + "-flush")
        if self.role == "stdout":
            if self.fault == "flush":
                raise self.error
            if self.fault == "late":
                self.context.end = time.monotonic() - 1
            if self.fault == "signal":
                self.context.signal = True


@final
class Peer:
    """Keep real socketpair I/O; only the already-connected address operation is inert."""

    def __init__(self, peer: socket.socket, context: Context, fault: str) -> None:
        """Own exactly this client endpoint and a separate checked-close fault fact."""
        self.peer, self.context, self.fault = peer, context, fault
        self.close_error = OSError("INERT_CHECKED_CLOSE_FAILURE")
        self.close_attempts = 0

    def connect(self, _path: str) -> None:
        """Use the actual preconnected pair instead of contacting any external endpoint."""
        self.context.trace.append("connected")

    def settimeout(self, timeout: float) -> None:
        """Keep every real operation finite and bounded by the original test interval."""
        self.peer.settimeout(min(timeout, 2))

    def send(self, data: bytes) -> int:
        """Write real bytes through the owned endpoint, including partial framing loops."""
        return self.peer.send(data[:7])

    def recv(self, count: int) -> bytes:
        """Read real bounded pieces so frame parsing must preserve the complete cursor."""
        return self.peer.recv(min(count, 11))

    def close(self) -> None:
        """Close the actual FD before injecting a separate close-status refusal."""
        self.close_attempts += 1
        self.peer.close()
        self.context.trace.append("client-close")
        if self.fault == "final-late":
            self.context.end = time.monotonic() - 1
        if self.fault == "final-signal":
            self.context.signal = True
        if self.fault in {"close", "write-close"}:
            raise self.close_error


def trial(  # noqa: C901, PLR0915 -- Closed protocol fault matrix; every owned peer settles first.
    fault: str = "deliver", mutation: Mapping[str, object] | None = None
) -> dict[str, object]:
    """Exercise actual client/Original/receipt/framing, then return only post-close facts."""
    with tempfile.TemporaryDirectory(prefix="ic-", dir="/tmp") as temporary, ExitStack() as stack:
        root = Path(temporary).resolve()
        root.chmod(0o700)
        write(root / "original", b"INERT_NEVER_EXECUTED", 0o755)
        value = receipt_value() | dict(mutation or {})
        raw = json.dumps(value, separators=(",", ":"), allow_nan=False).encode() + b"\n"
        write(root / "receipt.json", raw)
        receipt_sha = hashlib.sha256(raw).hexdigest()
        listener = stack.enter_context(SOCKET(socket.AF_UNIX, socket.SOCK_STREAM))
        left, right = socket.socketpair()
        _ = stack.callback(left.close)
        _ = stack.callback(right.close)
        listener.settimeout(2)
        left.settimeout(2)
        right.settimeout(2)
        listener.bind(str(root / "socket"))
        (root / "socket").chmod(0o600)
        context = Context(root)
        _ = stack.callback(context.close)
        peer = Peer(left, context, fault)
        output, error = Output(context, "stdout", fault), Output(context, "stderr", fault)
        server_facts: JsonObject = {}
        first: BaseException | None = None
        delivered: bool | None = None
        server_error: BaseException | None = None
        owned_inodes = {
            (metadata.st_dev, metadata.st_ino)
            for path in (root / "original", root / "receipt.json", root / "socket")
            for metadata in (LSTAT(path),)
        }

        def fstat(descriptor: int) -> os.stat_result:
            metadata = FSTAT(descriptor)
            return (
                mapped(metadata) if (metadata.st_dev, metadata.st_ino) in owned_inodes else metadata
            )

        def lstat(path: Path) -> os.stat_result:
            metadata = LSTAT(path)
            return (
                mapped(metadata) if (metadata.st_dev, metadata.st_ino) in owned_inodes else metadata
            )

        def check(deadline: float) -> None:
            if context.signal:
                raise context.signal_error
            CHECK(deadline)

        def server() -> BaseException | None:
            caught: BaseException | None = None
            end = time.monotonic() + 2
            try:
                request = injection.receive_json(right, end)
                server_facts["request"] = request
                context.trace.append("request")
                if fault == "disconnect":
                    return None
                response: dict[str, object] = {"action": "pass"}
                if fault != "pass":
                    response = {
                        "action": "deliver",
                        "stdoutBytes": len(OUT),
                        "stderrBytes": len(ERR),
                        "receiptSha256": "0" * 64 if fault == "receipt-sha" else receipt_sha,
                    }
                if fault == "unknown-reply":
                    response["PRIVATE-UNKNOWN"] = "INERT-PRIVATE-SECRET"
                injection.send_json(right, response, end)
                if fault != "pass":
                    injection.write_all(right, OUT + ERR, end)
                ack = injection.receive_json(right, end)
                server_facts["ack"] = ack
                context.trace.append("ack")
                if fault != "missing-completion":
                    complete: dict[str, object] = {
                        "action": "complete",
                        "nonce": "0" * 32 if fault == "wrong-completion" else NONCE,
                        "receiptSha256": None if fault == "pass" else receipt_sha,
                    }
                    if fault == "unknown-completion":
                        complete["PRIVATE-UNKNOWN"] = "INERT-PRIVATE-SECRET"
                    injection.send_json(right, complete, end)
                    context.trace.append("complete")
                right.shutdown(socket.SHUT_WR)
                context.trace.append("server-eof")
            except BaseException as caught_error:  # noqa: BLE001 -- Record before actual close.
                caught = caught_error
            finally:
                caught = ops_guest.settle((right.close,), caught)
            return caught

        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as executor:
            future = executor.submit(server)
            with (
                patch.object(injection, "SOCKET", root / "socket"),
                patch.object(injection, "RECEIPT", root / "receipt.json"),
                patch.object(socket, "socket", return_value=peer),
                patch.object(sys, "stdout", type("Sink", (), {"buffer": output})()),
                patch.object(sys, "stderr", type("Sink", (), {"buffer": error})()),
                patch.object(injection, "check", side_effect=check),
                patch.object(os, "fstat", side_effect=fstat),
                patch.object(Path, "lstat", autospec=True, side_effect=lstat),
            ):
                try:
                    delivered = injection.client(
                        cast("injection.Context", cast("object", context)), IDENTIFIER
                    )
                except BaseException as caught:  # noqa: BLE001 -- Close peers before assertions.
                    first = caught
                finally:
                    first = ops_guest.settle((context.close, left.close, listener.close), first)
            try:
                server_error = future.result(timeout=3)
            finally:
                right.close()
        closed: list[bool] = []
        for descriptor in context.descriptors:
            try:
                _ = FSTAT(descriptor)
            except OSError:
                closed.append(True)
            else:
                closed.append(False)
        result: dict[str, object] = {
            "failure": first,
            "delivered": delivered,
            "stdout": output.buffer.getvalue(),
            "stderr": error.buffer.getvalue(),
            "trace": context.trace,
            "server": server_facts,
            "serverError": server_error,
            "allOriginalsClosed": bool(closed) and all(closed),
            "originalCount": len(context.originals),
            "allSocketsClosed": all(item.fileno() == -1 for item in (left, right, listener)),
            "clientCloseAttempts": peer.close_attempts,
            "outputError": output.error,
            "closeError": peer.close_error,
            "signalError": context.signal_error,
            "receiptSha256": receipt_sha,
        }
        output.buffer.close()
        error.buffer.close()
    return result


def server(result: dict[str, object]) -> JsonObject:
    """Validate this test's JSON-only server facts apart from retained exception objects."""
    return object_value(json_value(result["server"]))


class InjectionClientTests(unittest.TestCase):
    """Positive delivery requires exact receipt, ACK, parent completion, EOF and checked close."""

    def settled(self, result: dict[str, object]) -> None:
        """Check every actual original and socket only after the executor has joined."""
        self.assertTrue(result["allOriginalsClosed"])
        self.assertTrue(result["allSocketsClosed"])
        self.assertEqual(result["clientCloseAttempts"], 1)

    def test_inspection_template_stays_exactly_bound_to_the_native_caller(self) -> None:
        """A caller-format change must not silently make the fixed barrier miss its role."""
        source = (ROOT / "ops/src/release/attestation/deploy/execution.rs").read_text()
        body = source.split("const CONTAINER: &str = concat!(", 1)[1].split(");", 1)[0]
        values: list[str] = []
        for line in body.splitlines():
            part = line.strip().removesuffix(",")
            if part:
                value = cast("object", json.loads(part))
                if not isinstance(value, str):
                    message = "native template literal required"
                    raise TypeError(message)
                values.append(value)
        self.assertEqual("".join(values), injection.NATIVE_FORMAT)

    def test_deliver_preserves_original_bytes_and_ack_precedes_parent_completion(self) -> None:
        """The complete real framed round-trip has no fabricated early success boundary."""
        result = trial()
        self.settled(result)
        self.assertIsNone(result["failure"])
        self.assertIsNone(result["serverError"])
        self.assertTrue(result["delivered"] is True)
        self.assertEqual(result["stdout"], OUT)
        self.assertEqual(result["stderr"], ERR)
        self.assertEqual(result["originalCount"], 2)
        self.assertEqual(
            obj(server(result), "request"),
            {"schemaVersion": 1, "nonce": NONCE, "identifier": IDENTIFIER},
        )
        self.assertEqual(
            obj(server(result), "ack"),
            {"action": "received", "nonce": NONCE, "receiptSha256": result["receiptSha256"]},
        )
        trace = cast("list[str]", result["trace"])
        self.assertLess(trace.index("stdout-flush"), trace.index("ack"))
        self.assertLess(trace.index("stderr-flush"), trace.index("ack"))
        self.assertLess(trace.index("ack"), trace.index("complete"))
        self.assertLess(trace.index("server-eof"), trace.index("client-close"))

    def test_pass_requires_ack_parent_completion_and_eof_without_reading_a_receipt(self) -> None:
        """Pass is only permission for later real-FD exec, not a fabricated Docker response."""
        result = trial("pass")
        self.settled(result)
        self.assertIsNone(result["failure"])
        self.assertIsNone(result["serverError"])
        self.assertTrue(result["delivered"] is False)
        self.assertEqual(result["stdout"], b"")
        self.assertEqual(result["stderr"], b"")
        self.assertEqual(result["originalCount"], 1)
        self.assertIsNone(obj(server(result), "ack")["receiptSha256"])

    def test_wrong_unknown_or_boolean_receipts_never_publish_raw_response_bytes(self) -> None:
        """Actual original receipt data must match every fixed parent effect and byte pin."""
        for mutation in (
            {"identifier": "e" * 64},
            {"image": "sha256:" + "e" * 64},
            {"nonce": "e" * 32},
            {"stopRawExit": True},
            {"responseRawExit": 7},
            {"runningAfter": True},
            {"stdoutSha256": "0" * 64},
            {"PRIVATE-UNKNOWN": "INERT-PRIVATE-SECRET"},
        ):
            with self.subTest(mutation=mutation):
                result = trial(mutation=mutation)
                self.settled(result)
                self.assertIsNotNone(result["failure"])
                self.assertEqual(result["stdout"], b"")
                self.assertEqual(result["stderr"], b"")
                self.assertNotIn("ack", server(result))
                self.assertNotIn("INERT-PRIVATE-SECRET", str(result["failure"]))

    def test_missing_wrong_or_unknown_parent_completion_invalidates_already_written_bytes(
        self,
    ) -> None:
        """ACK and bytes alone cannot stand in for the parent's explicit completion and EOF."""
        for fault in ("missing-completion", "wrong-completion", "unknown-completion"):
            with self.subTest(fault=fault):
                result = trial(fault)
                self.settled(result)
                self.assertIsNotNone(result["failure"])
                self.assertIsNone(result["delivered"])
                self.assertEqual(result["stdout"], OUT)
                self.assertEqual(result["stderr"], ERR)
                self.assertIn("ack", server(result))
                self.assertNotIn("INERT-PRIVATE-SECRET", str(result["failure"]))

    def test_response_hash_unknown_fields_or_disconnect_refuse_before_delivery(self) -> None:
        """A parent-shaped but incomplete/unbound response has no successful effect output."""
        for fault in ("receipt-sha", "unknown-reply", "disconnect"):
            with self.subTest(fault=fault):
                result = trial(fault)
                self.settled(result)
                self.assertIsNotNone(result["failure"])
                self.assertEqual(result["stdout"], b"")
                self.assertEqual(result["stderr"], b"")

    def test_partial_write_flush_and_secondary_socket_close_keep_first_failure(self) -> None:
        """Actual socket retirement cannot erase the first original-output refusal."""
        for fault in ("write", "short-write", "flush", "write-close", "close"):
            with self.subTest(fault=fault):
                result = trial(fault)
                self.settled(result)
                self.assertIsNotNone(result["failure"])
                if fault in {"write", "write-close", "flush"}:
                    self.assertIs(result["failure"], result["outputError"])
                if fault == "close":
                    self.assertIs(result["failure"], result["closeError"])
                    self.assertIn("complete", cast("list[str]", result["trace"]))
                if fault == "short-write":
                    self.assertEqual(result["stdout"], OUT[:-1])

    def test_late_or_latched_signal_at_output_and_after_actual_socket_close_stays_negative(
        self,
    ) -> None:
        """Supply only a lower signal observation; no process signal or handler is changed."""
        for fault in ("late", "signal", "final-late", "final-signal"):
            with self.subTest(fault=fault):
                result = trial(fault)
                self.settled(result)
                self.assertIsNotNone(result["failure"])
                self.assertIsNone(result["delivered"])
                if "signal" in fault:
                    self.assertIs(result["failure"], result["signalError"])


if __name__ == "__main__":
    _ = unittest.main()
