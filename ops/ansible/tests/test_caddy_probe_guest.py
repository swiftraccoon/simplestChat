"""Exercise prototype custody and containment decisions using only owned local files/fakes."""

from __future__ import annotations

import errno
import io
import ipaddress
import json
import os
import socket
import ssl
import tarfile
import tempfile
import time
import unittest
from contextlib import ExitStack
from pathlib import Path
from types import SimpleNamespace
from typing import TYPE_CHECKING, BinaryIO, cast, override
from unittest.mock import patch

from test_support import ROOT

# isort: split
import bounded_process
import caddy_materials
import caddy_probe_guest as guest

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping, Sequence

CID = "a" * 64


NETWORK = "b" * 64
ENGINE_IMAGE = "sha256:" + "c" * 64
BRIDGE = "sc0123456789ab"
ADDRESS = "172.19.0.2"
TLS_PHASE_END = 130.0
UNSPECIFIED = str(ipaddress.IPv4Address(0))


def transport_fixture() -> tuple[guest.Bridge, dict[str, object], dict[str, object]]:
    """Supply two joined inert daemon observations, never an actual daemon or packet."""
    bridge = guest.Bridge(NETWORK, BRIDGE, ENGINE_IMAGE)
    row: dict[str, object] = {
        "Id": CID,
        "Image": ENGINE_IMAGE,
        "Config": {"Image": guest.IMAGE},
        "HostConfig": {"NetworkMode": BRIDGE},
        "State": {
            "Running": True,
            "OOMKilled": False,
            "Status": "running",
            "Pid": 123,
            "Restarting": False,
            "Paused": False,
            "Dead": False,
        },
        "NetworkSettings": {
            "Networks": {
                BRIDGE: {
                    "NetworkID": NETWORK,
                    "EndpointID": "e" * 64,
                    "IPAddress": ADDRESS,
                    "IPPrefixLen": 16,
                    "Gateway": "172.19.0.1",
                }
            },
            "Ports": {"443/tcp": None, "443/udp": None, "80/tcp": None},
        },
    }
    network: dict[str, object] = {
        "Id": NETWORK,
        "Name": BRIDGE,
        "Driver": "bridge",
        "Scope": "local",
        "Internal": True,
        "EnableIPv6": False,
        "Options": {"com.docker.network.bridge.name": BRIDGE},
        "IPAM": {"Config": [{"Subnet": "172.19.0.0/16", "Gateway": "172.19.0.1"}]},
        "Containers": {CID: {"EndpointID": "e" * 64, "IPv4Address": ADDRESS + "/16"}},
    }
    return bridge, row, network


def terminal_fixture() -> tuple[guest.Bridge, dict[str, object], dict[str, object]]:
    """Supply a joined exact-owned exit with no usable address or live-network claim."""
    bridge, row, network = transport_fixture()
    row["HostConfig"] = {"NetworkMode": BRIDGE}
    row["State"] = {
        "Running": False,
        "Pid": 0,
        "Status": "exited",
        "ExitCode": 1,
        "Restarting": False,
        "Dead": False,
        "Paused": False,
        "OOMKilled": False,
    }
    row["NetworkSettings"] = {"Networks": {BRIDGE: {"NetworkID": NETWORK}}}
    network["Containers"] = {}
    return bridge, row, network


def backoff_fixture() -> tuple[guest.Bridge, dict[str, object], dict[str, object]]:
    """Supply Moby v28.2.2 SetRestarting/StateString-shaped metadata, not a terminal process."""
    bridge, row, network = terminal_fixture()
    guest.obj(row["State"]).update({"Running": True, "Restarting": True, "Status": "restarting"})
    return bridge, row, network


def phase_fixture(
    case: str, *, refused: bool
) -> tuple[guest.Bridge, dict[str, object], dict[str, object]]:
    """Use only the three fixed lower daemon states exercised by the phase trial."""
    if not refused:
        return transport_fixture()
    return backoff_fixture() if case == "backoff" else terminal_fixture()


def transport_target() -> guest.Endpoint:
    """Admit the same exact finite observation through the production pure policy."""
    return guest.endpoint_observation(CID, *transport_fixture())


def transport_report() -> guest.Report:
    """Provide only the fields used by the actual TLS diagnostic boundary."""
    return cast("guest.Report", cast("object", {"tlsCheckpoint": None, "tlsFailureCategory": None}))


class TransportSocket:
    """Keep a real local original while replacing only TLS/network observations."""

    def __init__(self, actual: socket.socket, clock: list[float], fault: str) -> None:
        """Retain an owned AF_UNIX socket; never perform a TLS handshake or network send."""
        self.actual: socket.socket = actual
        self.clock: list[float] = clock
        self.fault: str = fault
        self.request: bytes = b""
        self.timeouts: list[float] = []
        self.responses: list[io.BytesIO] = []
        self.primary: ssl.SSLCertVerificationError = ssl.SSLCertVerificationError(
            "INERT-PRIVATE-SSL"
        )

    def get_verified_chain(self) -> list[bytes]:
        """Return inert bytes for the public-only chain adapter."""
        if self.fault in {"ssl-refusal", "ssl-close", "ssl-late-close"}:
            self.clock[0] = 129.0 if self.fault == "ssl-late-close" else self.clock[0]
            raise self.primary
        return [b"leaf", b"intermediate", b"root"]

    def get_unverified_chain(self) -> list[bytes]:
        """Return the permitted leaf and held intermediate shape."""
        return [b"leaf", b"intermediate"]

    def sendall(self, raw: bytes) -> None:
        """Observe the exact fixed original request without actually sending."""
        self.request = raw
        if self.fault == "request-close":
            raise PermissionError(errno.EPERM, "INERT-PRIVATE-REQUEST")

    def settimeout(self, seconds: float) -> None:
        """Retain each clipped timeout for an independent bound assertion."""
        self.timeouts.append(seconds)

    def makefile(self, _mode: str) -> BinaryIO:
        """Feed the actual mature HTTP parser an inert bounded response."""
        response = io.BytesIO(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
        self.responses.append(response)
        return response

    def version(self) -> str:
        """Expose no real negotiated TLS claim from a fake."""
        return "INERT-TLS"

    def close(self) -> None:
        """Close the original before an optional late clock or close refusal."""
        self.actual.close()
        if self.fault in {"close-late", "ssl-late-close"}:
            self.clock[0] = 131.0
        if self.fault in {"close", "request-close", "ssl-close"}:
            message = "INERT-PRIVATE-CLOSE"
            raise OSError(message)


def transport_exchange(
    fault: str = "",
    phase_end: float = 130.0,
    consumer: str = "probe",
    *,
    clock: list[float] | None = None,
    recheck: Callable[[guest.Commands], None] | None = None,
) -> dict[str, object]:
    """Exercise the actual exchange/cleanup using owned local FDs and fake TLS boundaries."""
    clock = [100.0] if clock is None else clock
    control = guest.Control(100.0, 900, 60)
    target = transport_target()
    report = transport_report()
    raw, other = socket.socketpair()
    wrapped = TransportSocket(other, clock, fault)
    connect: list[tuple[tuple[str, int], float]] = []
    names: list[str] = []
    errors = (ssl.SSLCertVerificationError("INERT-PRIVATE-CERT"), OSError("INERT-PRIVATE-IO"))

    def connected(address: tuple[str, int], *, timeout: float) -> socket.socket:
        connect.append((address, timeout))
        if fault == "connect":
            raise errors[1]
        if fault == "connect-late":
            clock[0] = 131.0
        return raw

    def handshake(_raw: socket.socket, *, server_hostname: str) -> ssl.SSLSocket:
        names.append(server_hostname)
        if fault == "handshake":
            raise errors[0]
        return cast("ssl.SSLSocket", cast("object", wrapped))

    context = cast("ssl.SSLContext", cast("object", SimpleNamespace(wrap_socket=handshake)))
    failure: BaseException | None = None
    result: dict[str, guest.JsonValue] | None = None
    copy_outcome: tuple[str, guest.Refusal | None] | None = None
    try:
        with (
            patch.object(time, "monotonic", side_effect=lambda: clock[0]),
            patch.object(socket, "create_connection", side_effect=connected),
            patch.object(guest.Endpoint, "check", side_effect=recheck),
            patch.object(ssl, "create_default_context", return_value=context),
        ):
            try:
                commands = guest.Commands(Path("/unused"), control, owned=[CID])
                if consumer == "negative":
                    guest.negative_tls(commands, context, guest.DOMAIN, target, report)
                elif consumer == "wait":
                    result = guest.wait_tls(commands, context, target, report)
                elif consumer == "copy":
                    target, code = guest.negative_endpoint(commands, CID, target.bridge)
                    copy_outcome = guest.negative_copy_outcome(commands, target, report, code)
                else:
                    result = guest.tls_probe(
                        control, context, guest.DOMAIN, guest.TlsAttempt(target, report, phase_end)
                    )
            except BaseException as error:  # noqa: BLE001 -- Collect facts after all real closes.
                failure = error
        facts: dict[str, object] = {
            "failure": failure,
            "result": result,
            "connect": connect,
            "names": names,
            "request": wrapped.request,
            "timeouts": wrapped.timeouts,
            "responseClosed": all(item.closed for item in wrapped.responses),
            "rawClosed": raw.fileno() == -1,
            "wrappedClosed": other.fileno() == -1,
            "report": report,
            "errors": errors,
            "expectedPrimary": wrapped.primary,
            "copyOutcome": copy_outcome,
        }
    finally:
        raw.close()
        other.close()
    return facts


class CloseFault:
    """Close the actual local stream before reporting a checked-close fault."""

    def __init__(self, stream: BinaryIO) -> None:
        """Keep the actual original available for post-finalization facts."""
        self.stream: BinaryIO = stream

    def write(self, data: bytes) -> int:
        """Forward actual fixture bytes."""
        return self.stream.write(data)

    def flush(self) -> None:
        """Flush the actual fixture buffer."""
        self.stream.flush()

    def seek(self, offset: int) -> int:
        """Seek the actual original."""
        return self.stream.seek(offset)

    def read(self, size: int) -> bytes:
        """Read the actual captured fixture bytes."""
        return self.stream.read(size)

    def close(self) -> None:
        """Publish the real closed fact before the simulated completion error."""
        self.stream.close()
        message = "checked_close"
        raise OSError(message)


class FlushFault(CloseFault):
    """Expose an observed raw-zero effect before a failing evidence flush."""

    @override
    def flush(self) -> None:
        """Report a fixed simulated flush error."""
        message = "evidence_flush"
        raise OSError(message)

    @override
    def close(self) -> None:
        """Still close the actual admitted original."""
        self.stream.close()


def pump(data: bytes, status: int = 0) -> Callable[..., tuple[int, bytes, bytes]]:
    """Return a closed inert lower-adapter observation, without launching a child."""

    def run(
        _argv: Sequence[str],
        *,
        env: Mapping[str, str],
        output: BinaryIO,
        error: BinaryIO,
        limits: bounded_process.Limits,
    ) -> tuple[int, bytes, bytes]:
        del env, error, limits
        _ = output.write(data)
        return status, b"", b""

    return run


def counter_bytes(table: str, packets: int, size: int) -> bytes:
    """Provide only an inert fixed-table NFT counter snapshot plus its drop rule."""
    return json.dumps(
        {
            "nftables": [
                {
                    "counter": {
                        "family": "inet",
                        "table": table,
                        "name": "blocked_output",
                        "packets": packets,
                        "bytes": size,
                    }
                },
                {"rule": {"expr": [{"drop": None}]}},
            ]
        }
    ).encode()


class MarkerSocket:
    """Own a real local socket while replacing only its network-send boundary."""

    def __init__(
        self,
        actual: socket.socket,
        clock: list[float],
        fault: str,
        errors: tuple[OSError, OSError],
    ) -> None:
        """Keep explicit owned fixture state, never a runtime command or network target."""
        self.actual: socket.socket = actual
        self.clock: list[float] = clock
        self.fault: str = fault
        self.errors: tuple[OSError, OSError] = errors

    def sendto(self, data: bytes, address: tuple[str, int]) -> int:
        """Supply a deterministic kernel-style observation, never an actual packet."""
        if data != guest.MARKER or address != ("192.0.2.1", 53):
            message = "unexpected fixed marker"
            raise AssertionError(message)
        if self.fault in {"eperm", "eacces", "eperm-close"}:
            raise self.errors[0]
        if self.fault in {"eio", "eio-close"}:
            raise self.errors[1]
        if self.fault == "late":
            self.clock[0] = 150.0
        return len(data) - 1 if self.fault == "short" else len(data)

    def close(self) -> None:
        """Close the real original before an optional checked-close refusal."""
        self.actual.close()
        if self.fault in {"eperm-close", "eio-close"}:
            raise self.errors[1]


def marker_fixture(
    before: tuple[int, int], after: tuple[int, int], fault: str
) -> dict[str, object]:
    """Exercise only marker control flow and a real local Unix socket close, without sending."""
    table = "sc_caddy_" + "a" * 12
    actual = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
    descriptor = actual.fileno()
    clock = [100.0]
    control = guest.Control(100.0, 60, 10)
    calls: list[str] = []
    captures: list[bool] = []
    admitted: list[bool] = []
    report = cast("guest.Report", cast("object", {"ioCheckpoint": None}))
    send_error = PermissionError(errno.EPERM, "INERT-PRIVATE", "/INERT/PRIVATE")
    if fault == "eacces":
        send_error = PermissionError(errno.EACCES, "INERT-PRIVATE", "/INERT/PRIVATE")
    other_error = OSError(errno.EIO, "INERT-PRIVATE", "/INERT/PRIVATE")

    class Capture:
        """Model only a successful capture check; this is no pcap/runtime proof."""

        def check(self) -> None:
            """Record the required observation boundary without a child or capture."""
            captures.append(True)

    def factory(_family: int, _kind: int) -> MarkerSocket:
        admitted.append(True)
        return MarkerSocket(actual, clock, fault, (send_error, other_error))

    def call(name: str, _argv: Sequence[str]) -> bytes:
        calls.append(name)
        packets, size = before if name == "guard-before" else after
        return counter_bytes(table, packets, size)

    commands = cast(
        "guest.Commands",
        cast(
            "object",
            SimpleNamespace(
                control=control,
                watch=Capture(),
                call=call,
            ),
        ),
    )
    first: BaseException | None = None
    original_closed = False
    try:
        with (
            patch.object(socket, "socket", side_effect=factory),
            patch.object(time, "monotonic", side_effect=lambda: clock[0]),
        ):
            guest.guard_probe(commands, table, report)
    except BaseException as error:  # noqa: BLE001 -- Close the fixture's actual original afterward.
        first = error
    finally:
        try:
            _ = os.fstat(descriptor)
        except OSError as error:
            original_closed = error.errno == errno.EBADF
        actual.close()
    return {
        "failure": first,
        "sendError": send_error,
        "otherError": other_error,
        "closedBeforeFallback": original_closed,
        "closed": actual.fileno() == -1,
        "calls": calls,
        "captures": len(captures),
        "admitted": bool(admitted),
        "ioCheckpoint": report["ioCheckpoint"],
    }


class PrototypeTests(unittest.TestCase):
    """Keep every assertion after the actual local owners have been closed."""

    @classmethod
    @override
    def setUpClass(cls) -> None:
        """Require this suite to load the maintained module rather than a foreign flat import."""
        expected = ROOT / "build/caddy_probe_guest.py"
        if Path(guest.__file__).resolve() != expected:
            message = "maintained_caddy_module_source"
            raise AssertionError(message)

    def test_clock_keeps_original_epoch_and_strict_body_final_end(self) -> None:
        """Equality is late; entering cleanup never resets the original end."""
        control = guest.Control(100, 60, 10)
        with (
            patch.object(time, "monotonic", return_value=150),
            self.assertRaises(guest.PrototypeError),
        ):
            control.check()
        control.settling = True
        with patch.object(time, "monotonic", return_value=159):
            control.check()
        with (
            patch.object(time, "monotonic", return_value=160),
            self.assertRaises(guest.PrototypeError),
        ):
            control.check()
        self.assertEqual(control.end, 160)

    def test_firewall_covers_host_dns_forward_and_all_physical_egress(self) -> None:
        """An internal bridge alone never substitutes for host OUTPUT and link guards."""
        rules = guest.firewall("sc_caddy_0123456789ab", "ens3", "sc0123456789ab", "10.0.2.2")
        self.assertIn("hook output priority -1; policy drop", rules)
        self.assertIn("hook forward priority -1; policy drop", rules)
        self.assertIn('hook egress device "ens3" priority 0; policy drop', rules)
        self.assertIn("tcp sport 22 ct state established", rules)
        self.assertEqual(rules.count("ct state established"), 1)
        self.assertNotIn("udp dport 53 accept", rules)
        self.assertNotIn("\\", rules)
        for value in ("ens3; accept", "ens3\n"):
            with self.assertRaises(guest.PrototypeError):
                _ = guest.firewall("sc_caddy_0123456789ab", value, "owned", "10.0.2.2")

    def test_normal_arguments_keep_exact_mounts_uid_and_no_tls_override(self) -> None:
        """Prototype TLS is generated by the exact normal Caddyfile, never manual PEM binds."""
        arguments = guest.normal_arguments(
            Path("/owned"), Path("/owned/data"), Path("/owned/config"), "ownedbridge"
        )
        self.assertEqual(arguments[-1], guest.IMAGE)
        self.assertIn("10001:10001", arguments)
        self.assertIn("unless-stopped", arguments)
        self.assertIn(
            "type=bind,src=/owned/Caddyfile,dst=/etc/caddy/Caddyfile,readonly",
            arguments,
        )
        self.assertIn("type=bind,src=/owned/data,dst=/data", arguments)
        self.assertFalse(any("tls" in item or "issuer" in item for item in arguments))

    def test_known_create_id_survives_checked_stdout_close_failure(self) -> None:
        """Raw-zero complete natural output grants exact-ID cleanup before a later close fault."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            control = guest.Control(time.monotonic(), 60, 10)
            commands = guest.Commands(root, control, tools={"docker": "/never-executed"})
            streams: list[BinaryIO] = []
            original = guest.private_file

            def admitted(path: Path) -> BinaryIO:
                stream = original(path)
                streams.append(stream)
                return (
                    cast("BinaryIO", cast("object", CloseFault(stream)))
                    if len(streams) == 1
                    else stream
                )

            with (
                patch.object(guest, "private_file", side_effect=admitted),
                patch.object(
                    bounded_process,
                    "run",
                    side_effect=pump((CID + "\n").encode()),
                ),
                self.assertRaisesRegex(OSError, "^checked_close$"),
            ):
                _ = commands.create("create", [guest.IMAGE])
            closed = [stream.closed for stream in streams]
            known = tuple(commands.owned)
        self.assertEqual(closed, [True, True])
        self.assertEqual(known, (CID,))

    def test_known_create_effect_is_adopted_before_evidence_flush(self) -> None:
        """A flush failure cannot discard the complete raw-zero body observation."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            commands = guest.Commands(
                root, guest.Control(time.monotonic(), 60, 10), tools={"docker": "/never-executed"}
            )
            streams: list[BinaryIO] = []
            original = guest.private_file

            def admitted(path: Path) -> BinaryIO:
                stream = original(path)
                streams.append(stream)
                return (
                    cast("BinaryIO", cast("object", FlushFault(stream)))
                    if len(streams) == 1
                    else stream
                )

            with (
                patch.object(guest, "private_file", side_effect=admitted),
                patch.object(bounded_process, "run", side_effect=pump((CID + "\n").encode())),
                self.assertRaisesRegex(OSError, "^evidence_flush$"),
            ):
                _ = commands.create("create", [guest.IMAGE])
            closed, known = all(stream.closed for stream in streams), tuple(commands.owned)
            uncertain = commands.create_uncertain
        self.assertTrue(closed)
        self.assertEqual(known, (CID,))
        self.assertFalse(uncertain)

    def test_unknown_or_nonzero_create_never_authorizes_name_cleanup(self) -> None:
        """Complete-looking bytes with raw7, or a truncated raw0 ID, remain unknown."""
        for data, status in (((CID + "\n").encode(), 7), (CID.encode(), 0)):
            with (
                self.subTest(status=status),
                tempfile.TemporaryDirectory() as directory,
            ):
                commands = guest.Commands(
                    Path(directory),
                    guest.Control(time.monotonic(), 60, 10),
                    tools={"docker": "/never-executed"},
                )
                with (
                    patch.object(bounded_process, "run", side_effect=pump(data, status)),
                    self.assertRaises(guest.PrototypeError),
                ):
                    _ = commands.create("create", [guest.IMAGE])
                known = tuple(commands.owned)
                uncertain = commands.create_uncertain
                with self.assertRaisesRegex(guest.PrototypeError, "guard_retained"):
                    guest.check_guard_retained(commands, "inet", "sc_caddy_0123456789ab")
            self.assertEqual(known, ())
            self.assertTrue(uncertain)

    def test_second_capture_admission_failure_closes_first_original(self) -> None:
        """A later open failure cannot strand the first evidence owner."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            streams: list[BinaryIO] = []
            original = guest.private_file

            def admitted(path: Path) -> BinaryIO:
                if streams:
                    message = "second_admission"
                    raise OSError(message)
                stream = original(path)
                streams.append(stream)
                return stream

            commands = guest.Commands(
                root,
                guest.Control(time.monotonic(), 60, 10),
                tools={"docker": "/never-executed"},
            )
            with (
                patch.object(guest, "private_file", side_effect=admitted),
                self.assertRaisesRegex(OSError, "^second_admission$"),
            ):
                _ = commands.call("inert", ["docker", "version"])
            closed = streams[0].closed
        self.assertTrue(closed)

    def test_successful_remove_effect_precedes_evidence_close(self) -> None:
        """A known raw-zero removal is never retried because its log close failed."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            commands = guest.Commands(
                root,
                guest.Control(time.monotonic(), 60, 10),
                owned=[CID],
                tools={"docker": "/never-executed"},
            )
            streams: list[BinaryIO] = []
            original = guest.private_file

            def admitted(path: Path) -> BinaryIO:
                stream = original(path)
                streams.append(stream)
                return (
                    cast("BinaryIO", cast("object", CloseFault(stream)))
                    if len(streams) == 1
                    else stream
                )

            with (
                patch.object(guest, "private_file", side_effect=admitted),
                patch.object(
                    bounded_process,
                    "run",
                    side_effect=pump((CID + "\n").encode()),
                ),
                self.assertRaises(OSError),
            ):
                commands.remove(CID)
            closed, known = (
                all(stream.closed for stream in streams),
                tuple(commands.owned),
            )
        self.assertTrue(closed)
        self.assertEqual(known, ())

    def test_guard_is_retained_when_any_known_container_cannot_retire(self) -> None:
        """No failed cleanup may open outbound contact for an unretired Caddy."""
        with tempfile.TemporaryDirectory() as directory:
            commands = guest.Commands(
                Path(directory),
                guest.Control(time.monotonic(), 60, 10),
                owned=[CID],
                tools={"nft": "/never-executed"},
            )
            with (
                patch.object(bounded_process, "run") as run,
                self.assertRaisesRegex(guest.PrototypeError, "guard_retained"),
            ):
                guest.check_guard_retained(commands, "inet", "sc_caddy_0123456789ab")
            called = run.called
        self.assertFalse(called)

    def test_guard_positive_observation_never_deletes_guest_confinement(self) -> None:
        """Only the owned QEMU exit retires the guard, including delayed resolver work."""
        with tempfile.TemporaryDirectory() as directory:
            commands = guest.Commands(Path(directory), guest.Control(time.monotonic(), 60, 10))
            with patch.object(guest.Commands, "call", return_value=b'{"drop":null}') as observed:
                for family in ("inet", "netdev"):
                    guest.check_guard_retained(commands, family, "sc_caddy_0123456789ab")
                arguments = [cast("list[str]", call.args[1]) for call in observed.call_args_list]
        self.assertEqual(len(arguments), 2)
        for argument in arguments:
            self.assertIn("list", argument)
            self.assertNotIn("delete", argument)

    def test_bounded_original_does_not_follow_an_ancestor_alias(self) -> None:
        """Canonical material admission refuses an otherwise valid final-parent symlink route."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            actual = root / "actual"
            actual.mkdir()
            guest.write_file(actual / "leaf", b"fixture")
            (root / "alias").symlink_to(actual, target_is_directory=True)
            with self.assertRaisesRegex(guest.PrototypeError, "^material_spelling$"):
                _ = guest.Original.open(
                    root / "alias/leaf", guest.Control(time.monotonic(), 60, 10)
                )
            original = guest.bounded_original(
                actual / "leaf", 32, guest.Control(time.monotonic(), 60, 10)
            )
        self.assertEqual(original, b"fixture")

    def test_actual_material_change_refuses_and_both_originals_close(self) -> None:
        """Compare actual original metadata/bytes; do not substitute a reopened stream."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            path = root / "material"
            guest.write_file(path, b"original")
            parent = os.open(root, os.O_RDONLY | os.O_DIRECTORY)
            fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
            original = guest.Original(parent, fd, path, os.fstat(fd), os.fstat(parent), b"original")
            control = guest.Control(time.monotonic(), 60, 10)
            path.chmod(0o640)
            first = None
            try:
                original.check(control)
            except guest.PrototypeError as error:
                first = error
            failure = guest.settle((original.close,), first)
            descriptors_closed: list[bool] = []
            for current in (parent, fd):
                try:
                    _ = os.fstat(current)
                except OSError:
                    descriptors_closed.append(True)
        self.assertIs(failure, first)
        self.assertEqual(str(failure), "material_changed")
        self.assertEqual(descriptors_closed, [True, True])

    def test_existing_trust_file_is_never_adopted_or_unlinked(self) -> None:
        """Exclusive failure preserves the preexisting foreign public-root candidate."""
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory).resolve() / "foreign.crt"
            guest.write_file(path, b"foreign")
            with self.assertRaises(FileExistsError):
                _ = guest.TrustCopy.create(
                    path, b"candidate", guest.Control(time.monotonic(), 60, 10)
                )
            content = path.read_bytes()
        self.assertEqual(content, b"foreign")

    def test_changed_trust_copy_closes_peers_without_foreign_unlink(self) -> None:
        """A changed guest-only trust entry remains negative through both original closes."""
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory).resolve() / "root.crt"
            control = guest.Control(time.monotonic(), 60, 10)
            owner = guest.TrustCopy.create(path, b"public-root", control)
            path.chmod(0o600)
            with self.assertRaisesRegex(guest.PrototypeError, "^material_changed$"):
                owner.finish(control)
            closed, retained = owner.file.closed, path.exists()
            with self.assertRaises(OSError):
                _ = os.fstat(owner.view.parent)
        self.assertTrue(closed)
        self.assertTrue(retained)

    def test_binary_copy_requires_one_regular_exact_member(self) -> None:
        """Mature TAR reads are bounded; traversal names and extra members never become files."""
        for names in (("caddy",), ("../caddy",), ("caddy", "extra")):
            capture = io.BytesIO()
            with tarfile.open(fileobj=capture, mode="w", format=tarfile.USTAR_FORMAT) as archive:
                for name in names:
                    member = tarfile.TarInfo(name)
                    member.size = 3
                    archive.addfile(member, io.BytesIO(b"ELF"))
            with tempfile.TemporaryDirectory() as directory:
                commands = guest.Commands(Path(directory), guest.Control(time.monotonic(), 60, 10))
                with patch.object(guest.Commands, "call", return_value=capture.getvalue()):
                    if len(names) == 1 and names[0] == "caddy":
                        result = guest.binary_digest(commands, CID)
                        self.assertEqual(len(result), 64)
                    else:
                        with self.assertRaises(guest.PrototypeError):
                            _ = guest.binary_digest(commands, CID)

    def test_both_public_chain_observations_bind_only_held_certificates(self) -> None:
        """A trusted reconstructed chain cannot hide a different served leaf/intermediate."""
        facts: dict[str, guest.JsonValue] = {
            name: {"certificateSha256": value}
            for name, value in (
                ("leaf", "leaf"),
                ("intermediate", "intermediate"),
                ("root", "root"),
            )
        }
        for served in (["leaf", "intermediate"], ["leaf", "intermediate", "root"]):
            guest.bind_live_chain(
                {
                    "verifiedChainSha256": ["leaf", "intermediate", "root"],
                    "servedChainSha256": list(served),
                },
                facts,
            )
        for served in (["leaf", "foreign"], ["leaf"], ["foreign", "intermediate", "root"]):
            with self.assertRaisesRegex(guest.PrototypeError, "^held_served_chain$"):
                guest.bind_live_chain(
                    {
                        "verifiedChainSha256": ["leaf", "intermediate", "root"],
                        "servedChainSha256": list(served),
                    },
                    facts,
                )
        with self.assertRaisesRegex(guest.PrototypeError, "^held_tls_chain$"):
            guest.bind_live_chain(
                {
                    "verifiedChainSha256": ["foreign", "intermediate", "root"],
                    "servedChainSha256": ["leaf", "intermediate"],
                },
                facts,
            )

    def test_original_validator_keeps_network_none_and_both_eight_mib_tmpfs(self) -> None:
        """Execute the actual finite validator argument builder without a daemon."""
        with tempfile.TemporaryDirectory() as directory:
            commands = guest.Commands(Path(directory), guest.Control(time.monotonic(), 60, 10))
            with (
                patch.object(guest.Commands, "create", return_value=CID) as create,
                patch.object(guest.Commands, "call", return_value=b"0\n"),
                patch.object(guest.Commands, "remove"),
            ):
                row = guest.validate(commands)
            arguments = cast("list[str]", create.call_args.args[1])
        self.assertIn("none", arguments)
        self.assertIn("/data:rw,uid=10001,gid=10001,size=8m", arguments)
        self.assertIn("/config:rw,uid=10001,gid=10001,size=8m", arguments)
        self.assertEqual(
            arguments[-5:], [guest.IMAGE, "caddy", "validate", "--config", "/etc/caddy/Caddyfile"]
        )
        self.assertEqual(row["exitStatus"], 0)

    def test_public_chain_adapter_validates_runtime_api_and_returned_bytes(self) -> None:
        """Public3.13 behavior is required at runtime without widening the typing baseline."""
        valid = SimpleNamespace(get_verified_chain=lambda: [b"leaf", b"intermediate", b"root"])
        result = guest.public_chain(
            cast("ssl.SSLSocket", cast("object", valid)), "get_verified_chain"
        )
        self.assertEqual(result, [b"leaf", b"intermediate", b"root"])
        for returned in (None, b"not-an-array", [], [b"leaf", "not-bytes"], [b"x"] * 4):
            invalid = SimpleNamespace(get_verified_chain=lambda value=returned: value)
            with self.assertRaises(guest.PrototypeError):
                _ = guest.public_chain(
                    cast("ssl.SSLSocket", cast("object", invalid)), "get_verified_chain"
                )
        with self.assertRaisesRegex(guest.PrototypeError, "^python_public_chain_api_missing$"):
            _ = guest.public_chain(
                cast("ssl.SSLSocket", cast("object", SimpleNamespace())), "get_verified_chain"
            )

    def test_negative_tls_does_not_swallow_deadline_refusal(self) -> None:
        """A timeout or cancellation cannot be counted as a wrong-trust proof."""
        control = guest.Control(time.monotonic(), 60, 10)
        commands = guest.Commands(Path("/unused"), control, owned=[CID])
        target = transport_target()
        report = transport_report()
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        with (
            patch.object(guest.Endpoint, "check"),
            patch.object(guest, "tls_probe", side_effect=guest.PrototypeError("guest_deadline")),
            self.assertRaisesRegex(guest.PrototypeError, "^guest_deadline$"),
        ):
            guest.negative_tls(commands, context, guest.DOMAIN, target, report)

    def test_marker_eperm_needs_fresh_named_counter_and_actual_socket_close(self) -> None:
        """Accept only the fixed denial observation plus a fresh single-packet delta and capture."""
        for fault in ("eperm", "success"):
            with self.subTest(fault=fault):
                value = marker_fixture((4, 200), (5, 250), fault)
                self.assertIsNone(value["failure"])
                self.assertTrue(value["closedBeforeFallback"])
                self.assertTrue(value["closed"])
                self.assertEqual(value["calls"], ["guard-before", "guard-after"])
                self.assertEqual(value["captures"], 2)

    def test_marker_eperm_never_accepts_stale_counter_extra_packets_or_empty_byte_delta(
        self,
    ) -> None:
        """A preexisting drop or unrelated activity cannot stand in for this fixed attempt."""
        for after in ((4, 200), (6, 300), (5, 200)):
            with self.subTest(after=after):
                value = marker_fixture((4, 200), after, "eperm")
                self.assertIsInstance(value["failure"], guest.PrototypeError)
                self.assertTrue(value["closedBeforeFallback"])
                self.assertTrue(value["closed"])
                self.assertEqual(value["ioCheckpoint"], "containment-counter-validate")

    def test_unrelated_marker_io_and_checked_close_refusals_remain_negative(self) -> None:
        """Preserve the actual first object through an actual socket close; waive no other errno."""
        for fault in ("eacces", "eio", "eio-close", "eperm-close"):
            with self.subTest(fault=fault):
                value = marker_fixture((4, 200), (5, 250), fault)
                expected = value["sendError"] if fault == "eacces" else value["otherError"]
                self.assertIs(value["failure"], expected)
                self.assertEqual(
                    guest.errno_category(cast("BaseException", value["failure"])),
                    "EACCES" if fault == "eacces" else "EIO",
                )
                self.assertTrue(value["closedBeforeFallback"])
                self.assertTrue(value["closed"])
                self.assertEqual(value["calls"], ["guard-before"])
                if fault == "eperm-close":
                    self.assertEqual(value["ioCheckpoint"], "containment-marker-close")
                else:
                    self.assertEqual(value["ioCheckpoint"], "containment-marker-send")

    def test_marker_short_send_or_original_body_end_never_becomes_guard_success(self) -> None:
        """Fence actual supplied completion and strict original epoch after the send returns."""
        for fault in ("short", "late"):
            with self.subTest(fault=fault):
                value = marker_fixture((4, 200), (5, 250), fault)
                self.assertIsInstance(value["failure"], guest.PrototypeError)
                self.assertTrue(value["closedBeforeFallback"])
                self.assertTrue(value["closed"])
                self.assertEqual(value["calls"], ["guard-before"])

    def test_named_counter_requires_exact_table_single_record_and_unsigned_numeric_fields(
        self,
    ) -> None:
        """Reject ambiguous witnesses rather than treating any named number as proof."""
        table = "sc_caddy_" + "a" * 12
        original: dict[str, object] = {
            "family": "inet",
            "table": table,
            "name": "blocked_output",
            "packets": 4,
            "bytes": 200,
        }
        for mutation in (
            {"table": "other"},
            {"family": "netdev"},
            {"packets": True},
            {"bytes": -1},
            {"packets": 2**64},
            {"name": "other"},
        ):
            value = original | mutation
            raw = json.dumps({"nftables": [{"counter": value}]}).encode()
            with self.subTest(mutation=mutation), self.assertRaises(guest.PrototypeError):
                _ = guest.blocked_counter(raw, table)
        duplicate = json.dumps(
            {"nftables": [{"counter": original}, {"counter": original}]}
        ).encode()
        with self.assertRaises(guest.PrototypeError):
            _ = guest.blocked_counter(duplicate, table)


class InternalBridgeTransportTests(unittest.TestCase):
    """Exercise only closed lower daemon/TLS observations, never an engine or live TLS peer."""

    def test_endpoint_accepts_only_joined_private_bridge_without_claiming_publication(self) -> None:
        """Requested loopback ports may be operationally absent on an internal network."""
        bridge, row, network = transport_fixture()
        target = guest.endpoint_observation(CID, bridge, row, network)
        self.assertEqual(target.address, ADDRESS)
        self.assertEqual(
            target.facts(),
            {
                "transport": "owned-internal-bridge-ipv4",
                "operationalLoopbackBindingCount": 0,
                "publishedLoopbackPathProved": False,
            },
        )
        port_cases: tuple[tuple[object, int], ...] = (
            (None, 0),
            ({}, 0),
            ({"443/tcp": []}, 0),
            ({"2019/tcp": None}, 0),
            ({"2019/tcp": []}, 0),
            ({"2019/tcp": None, "443/tcp": [{"HostIp": "127.0.0.1", "HostPort": "443"}]}, 1),
            ({"443/tcp": [{"HostIp": "127.0.0.1", "HostPort": "443"}]}, 1),
        )
        for ports, expected in port_cases:
            self.assertEqual(guest.operational_bindings(ports), expected)

    def test_internal_endpoint_gateway_may_be_empty_or_exact_canonical_ipam_gateway(self) -> None:
        """An absent internal default route must not alter the joined target or IPAM exclusion."""
        expected = transport_target()
        for gateway in ("", "172.19.0.1"):
            with self.subTest(gateway=gateway):
                bridge, row, network = transport_fixture()
                member = guest.obj(guest.obj(guest.obj(row["NetworkSettings"])["Networks"])[BRIDGE])
                member["Gateway"] = gateway
                self.assertEqual(guest.endpoint_observation(CID, bridge, row, network), expected)

    def test_bad_or_missing_endpoint_gateway_is_distinct_and_does_not_weaken_ipam(self) -> None:
        """Null/unknown/default-route data stay refused without exposing actual address strings."""
        gateways: tuple[object, ...] = (
            None,
            False,
            0,
            [],
            {},
            "172.19.0.254",
            "172.019.0.1",
            "missing",
        )
        for gateway in gateways:
            with self.subTest(gateway=gateway):
                bridge, row, network = transport_fixture()
                member = guest.obj(guest.obj(guest.obj(row["NetworkSettings"])["Networks"])[BRIDGE])
                if gateway == "missing":
                    del member["Gateway"]
                else:
                    member["Gateway"] = gateway
                with self.assertRaisesRegex(guest.PrototypeError, "^endpoint_gateway$"):
                    _ = guest.endpoint_observation(CID, bridge, row, network)
        bridge, row, network = transport_fixture()
        member = guest.obj(guest.obj(guest.obj(row["NetworkSettings"])["Networks"])[BRIDGE])
        member.update({"Gateway": "", "IPAddress": "172.19.0.1"})
        with self.assertRaisesRegex(guest.PrototypeError, "^endpoint_ipv4$"):
            _ = guest.endpoint_observation(CID, bridge, row, network)
        member["IPAddress"] = ADDRESS
        guest.obj(network["IPAM"])["Config"] = [
            {"Subnet": "172.19.0.0/16", "Gateway": "172.019.0.1"},
        ]
        with self.assertRaises(ValueError):
            _ = guest.endpoint_observation(CID, bridge, row, network)

    def test_exposed_admin_port_may_be_unbound_but_never_host_published(self) -> None:
        """The pinned base EXPOSE metadata cannot authorize any admin binding or unknown port."""
        for host in ("127.0.0.1", UNSPECIFIED, ""):
            with self.subTest(host=host), self.assertRaises(guest.PrototypeError):
                _ = guest.operational_bindings(
                    {
                        "2019/tcp": [{"HostIp": host, "HostPort": "2019"}],
                    }
                )
        with self.assertRaises(guest.PrototypeError):
            _ = guest.operational_bindings({"9000/tcp": None})

    def test_endpoint_rejects_wrong_identity_network_state_and_foreign_publication(self) -> None:
        """A known name, requested port or equal-looking address is never sufficient authority."""
        cases: tuple[tuple[str, tuple[str, ...], object], ...] = (
            ("container", ("Id",), "f" * 64),
            ("container", ("Image",), "sha256:" + "f" * 64),
            ("container", ("Config", "Image"), "foreign-image"),
            ("container", ("State", "Running"), False),
            ("container", ("State", "OOMKilled"), True),
            ("container", ("NetworkSettings", "Networks"), {}),
            ("container", ("NetworkSettings", "Networks", BRIDGE, "NetworkID"), "f" * 64),
            ("container", ("NetworkSettings", "Networks", BRIDGE, "EndpointID"), "f" * 64),
            ("network", ("Id",), "f" * 64),
            ("network", ("Name",), "foreign-network"),
            ("network", ("Internal",), False),
            ("network", ("Driver",), "host"),
            ("network", ("EnableIPv6",), True),
            ("network", ("Containers",), {}),
            ("network", ("Containers", CID, "IPv4Address"), "172.19.0.3/16"),
            (
                "container",
                ("NetworkSettings", "Ports"),
                {
                    "443/tcp": [{"HostIp": UNSPECIFIED, "HostPort": "443"}],
                },
            ),
        )
        for name, path, replacement in cases:
            with self.subTest(name=name, path=path):
                bridge, row, network = transport_fixture()
                current = row if name == "container" else network
                for key in path[:-1]:
                    current = guest.obj(current[key])
                current[path[-1]] = replacement
                with self.assertRaises(guest.PrototypeError):
                    _ = guest.endpoint_observation(CID, bridge, row, network)

    def test_endpoint_rejects_unusable_noncanonical_out_of_subnet_and_control_peer_ip(self) -> None:
        """Reject ambient names, foreign subnets, special addresses and the SSH control peer."""
        for address in (
            "localhost",
            "127.0.0.1",
            "169.254.1.2",
            "224.0.0.1",
            UNSPECIFIED,
            "8.8.8.8",
            "172.20.0.2",
            "172.19.0.0",
            "172.19.255.255",
            "172.19.0.1",
            "172.019.0.2",
            "::1",
        ):
            with self.subTest(address=address):
                bridge, row, network = transport_fixture()
                member = guest.obj(guest.obj(guest.obj(row["NetworkSettings"])["Networks"])[BRIDGE])
                member["IPAddress"] = address
                with self.assertRaises((guest.PrototypeError, ValueError)):
                    _ = guest.endpoint_observation(CID, bridge, row, network)
        bridge, row, network = transport_fixture()
        member = guest.obj(guest.obj(guest.obj(row["NetworkSettings"])["Networks"])[BRIDGE])
        member.update({"IPAddress": "10.0.2.2", "Gateway": "10.0.0.1", "IPPrefixLen": 8})
        network["IPAM"] = {"Config": [{"Subnet": "10.0.0.0/8", "Gateway": "10.0.0.1"}]}
        guest.obj(network["Containers"])[CID] = {
            "EndpointID": "e" * 64,
            "IPv4Address": "10.0.2.2/8",
        }
        with self.assertRaisesRegex(guest.PrototypeError, "^endpoint_ipv4$"):
            _ = guest.endpoint_observation(CID, bridge, row, network)

    def test_owned_endpoint_queries_exact_ids_and_refuses_stale_or_late_observation(self) -> None:
        """Both metadata queries and final live checks precede any address adoption."""
        bridge, row, network = transport_fixture()
        clock = [100.0]
        commands = guest.Commands(Path("/unused"), guest.Control(100.0, 60, 10), owned=[CID])
        calls: list[list[str]] = []

        def observed(_role: str, argv: Sequence[str]) -> bytes:
            calls.append(list(argv))
            return json.dumps([network if "network" in argv else row]).encode()

        with (
            patch.object(time, "monotonic", side_effect=lambda: clock[0]),
            patch.object(guest.Commands, "call", side_effect=observed),
        ):
            target = guest.owned_endpoint(commands, CID, bridge)
            self.assertEqual(
                calls,
                [
                    ["docker", "inspect", CID],
                    ["docker", "network", "inspect", NETWORK],
                ],
            )
            member = guest.obj(guest.obj(guest.obj(row["NetworkSettings"])["Networks"])[BRIDGE])
            member["IPAddress"] = "172.19.0.3"
            guest.obj(guest.obj(network["Containers"])[CID])["IPv4Address"] = "172.19.0.3/16"
            with self.assertRaisesRegex(guest.PrototypeError, "^endpoint_changed$"):
                target.check(commands)
            clock[0] = 151.0
            before = len(calls)
            with self.assertRaisesRegex(guest.PrototypeError, "^guest_deadline$"):
                _ = guest.owned_endpoint(commands, CID, bridge)
            self.assertEqual(len(calls), before)
            clock[0] = 100.0
            commands.owned.clear()
            with self.assertRaisesRegex(guest.PrototypeError, "^foreign_container$"):
                _ = guest.owned_endpoint(commands, CID, bridge)
            self.assertEqual(len(calls), before)

    def test_transport_changes_only_numeric_destination_and_clips_all_io_timeouts(self) -> None:
        """The real HTTP parser sees the original Host/request; trust and SNI are untouched."""
        value = transport_exchange(phase_end=102.0)
        self.assertIsNone(value["failure"])
        self.assertEqual(value["connect"], [((ADDRESS, 443), 2.0)])
        self.assertEqual(value["names"], [guest.DOMAIN])
        self.assertEqual(
            value["request"],
            b"GET /metrics HTTP/1.1\r\nHost: maintain.localhost\r\nConnection: close\r\n\r\n",
        )
        self.assertEqual(set(cast("list[float]", value["timeouts"])), {2.0})
        self.assertTrue(value["rawClosed"])
        self.assertTrue(value["wrappedClosed"])
        self.assertTrue(value["responseClosed"])
        self.assertEqual(guest.MAX_COMMANDS, 128)

    def test_tls_failures_keep_closed_stage_and_first_cause_after_all_known_peer_closes(
        self,
    ) -> None:
        """Keep the first failed site across connection, trust and checked-close failures."""
        for fault, stage, category in (
            ("connect", "connect", "io"),
            ("handshake", "handshake", "certificate-verification"),
            ("request-close", "request", "permission"),
            ("close", "close", "io"),
            ("connect-late", "connect", "policy"),
            ("close-late", "close", "policy"),
        ):
            with self.subTest(fault=fault):
                value = transport_exchange(fault)
                self.assertIsNotNone(value["failure"])
                report = cast("dict[str, object]", value["report"])
                self.assertEqual(
                    (report["tlsCheckpoint"], report["tlsFailureCategory"]), (stage, category)
                )
                self.assertNotIn("INERT-PRIVATE", json.dumps(report))
                self.assertTrue(value["responseClosed"])
                if fault != "connect":
                    self.assertTrue(value["rawClosed"])
                if fault not in {"connect", "handshake", "connect-late"}:
                    self.assertTrue(value["wrappedClosed"])
                if fault == "request-close":
                    self.assertIsInstance(value["failure"], PermissionError)

    def test_wait_tls_never_accepts_late_success_or_starts_an_expired_next_probe(self) -> None:
        """Refuse the old-helper late-success counterexample and expired retries."""
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        for case in ("late-success", "late-recheck", "expired"):
            with self.subTest(case=case):
                clock = [100.0]
                control = guest.Control(100.0, 900, 60)
                commands = guest.Commands(Path("/unused"), control, owned=[CID])
                probes: list[float] = []

                def probe(
                    _control: guest.Control,
                    _context: ssl.SSLContext,
                    _hostname: str,
                    _attempt: guest.TlsAttempt,
                    *,
                    state: tuple[list[float], str, list[float]] = (clock, case, probes),
                ) -> dict[str, guest.JsonValue]:
                    current_clock, current_case, current_probes = state
                    _attempt.peers_closed = True
                    _attempt.phase_complete = True
                    current_probes.append(current_clock[0])
                    if current_case == "expired":
                        current_clock[0] = 130.0
                        raise ConnectionRefusedError
                    current_clock[0] = 131.0 if current_case == "late-success" else 129.0
                    return {"status": 404}

                def recheck(
                    _commands: guest.Commands,
                    *,
                    state: tuple[list[float], str] = (clock, case),
                ) -> None:
                    current_clock, current_case = state
                    if current_case == "late-recheck":
                        current_clock[0] = 131.0

                def now(current: list[float] = clock) -> float:
                    return current[0]

                with (
                    patch.object(time, "monotonic", side_effect=now),
                    patch.object(guest, "tls_probe", side_effect=probe),
                    patch.object(guest.Endpoint, "check", side_effect=recheck),
                    self.assertRaisesRegex(
                        guest.PrototypeError,
                        "^tls_not_ready$" if case == "expired" else "^tls_deadline$",
                    ),
                ):
                    _ = guest.wait_tls(commands, context, transport_target(), transport_report())
                self.assertEqual(probes, [100.0])


def terminal_daemon(calls: list[str]) -> Callable[..., bytes]:
    """Return only fixed negative-copy daemon observations, with no subprocess or engine."""

    def call(
        role: str,
        _argv: Sequence[str],
        _seconds: float = 30,
        *,
        adopt: Callable[[bytes], None] | None = None,
    ) -> bytes:
        calls.append(role)
        if role == "negative-create":
            raw = (CID + "\n").encode()
        elif role == "negative-inspect":
            _, row, _ = terminal_fixture()
            raw = json.dumps([row]).encode()
        elif role == "negative-network":
            _, _, network = terminal_fixture()
            raw = json.dumps([network]).encode()
        elif role in {"negative-start", "negative-stop", "remove"}:
            raw = b""
        else:
            message = "unexpected inert role"
            raise AssertionError(message)
        if adopt is not None:
            adopt(raw)
        return raw

    return call


def copy_phase_exchange(case: str) -> dict[str, object]:
    """Observe a real clean-refusal phase plus a fresh fixed daemon transition, without live TLS."""
    clock = [100.0]
    terminal = [False]
    queries: list[list[str]] = []
    original_check = guest.Endpoint.check
    original_control_check = guest.Control.check
    expected = transport_target()

    def observation(_role: str, argv: Sequence[str]) -> bytes:
        queries.append(list(argv))
        if terminal[0] and case == "global-expiry":
            clock[0] = 941.0
        _, row, network = phase_fixture(case, refused=terminal[0])
        replacements: dict[str, tuple[dict[str, object], str, str]] = {
            "wrong-cid": (row, "Id", "f" * 64),
            "wrong-image": (row, "Image", "sha256:" + "f" * 64),
            "wrong-network": (network, "Id", "f" * 64),
        }
        if terminal[0] and case in replacements:
            target, key, value = replacements[case]
            target[key] = value
        return json.dumps([network if "network" in argv else row]).encode()

    def sleep(_seconds: float) -> None:
        # Model elapsed scheduling time after the actual clean refusal, never reset the end.
        clock[0] = 130.0
        terminal[0] = True

    def recheck(commands: guest.Commands) -> None:
        if case == "successful-tls":
            terminal[0] = True
        original_check(expected, commands)

    def checked(control: guest.Control) -> None:
        if case == "signal" and clock[0] >= TLS_PHASE_END:
            control.cancelled = True
        original_control_check(control)

    fault = {
        "successful-tls": "",
        "late-close": "ssl-late-close",
        "failed-close": "ssl-close",
    }.get(case, "ssl-refusal")
    with (
        patch.object(guest.Commands, "call", side_effect=observation),
        patch.object(time, "sleep", side_effect=sleep),
        patch.object(guest.Control, "check", checked),
    ):
        result = transport_exchange(fault, consumer="copy", clock=clock, recheck=recheck)
    result["queries"] = queries
    return result


def descriptor_closed(descriptor: int) -> bool:
    """Observe the actual original after all explicit test-owner finalizers have returned."""
    try:
        _ = os.fstat(descriptor)
    except OSError as error:
        return error.errno == errno.EBADF
    return False


class CaddyNegativeSettlementTests(unittest.TestCase):
    """Separate exact-owned terminal refusal from live transport and checked-close authority."""

    def test_terminal_refusal_has_no_endpoint_and_wrong_terminal_identity_never_qualifies(
        self,
    ) -> None:
        """Only observed exited/nonzero/non-OOM exact-owned instances may skip a live TLS probe."""
        control = guest.Control(time.monotonic(), 900, 60)
        commands = guest.Commands(Path("/unused"), control, owned=[CID])
        mutations: tuple[tuple[str, object], ...] = (
            ("accepted", None),
            ("ExitCode", 0),
            ("ExitCode", True),
            ("ExitCode", None),
            ("Restarting", True),
            ("Dead", True),
            ("Paused", True),
            ("OOMKilled", True),
            ("Status", "created"),
            ("identity", "f" * 64),
            ("network", "f" * 64),
        )
        for key, value in mutations:
            with self.subTest(key=key):
                bridge, row, network = terminal_fixture()
                if key == "identity":
                    row["Id"] = value
                elif key == "network":
                    network["Id"] = value
                elif key != "accepted":
                    guest.obj(row["State"])[key] = value
                with patch.object(
                    guest.Commands,
                    "call",
                    side_effect=[
                        json.dumps([row]).encode(),
                        json.dumps([network]).encode(),
                    ],
                ):
                    if key == "accepted":
                        self.assertEqual(
                            guest.negative_endpoint(commands, CID, bridge),
                            (None, guest.Refusal("exited", 1)),
                        )
                    else:
                        with self.assertRaises(guest.PrototypeError):
                            _ = guest.negative_endpoint(commands, CID, bridge)
                with self.assertRaises(guest.PrototypeError):
                    _ = guest.endpoint_observation(CID, bridge, row, network)

    def test_negative_copies_can_observe_exact_owned_exit_without_constructing_tls(self) -> None:
        """Run the full negative-copy loop with real retained files and inert daemon data."""
        descriptors: list[int] = []
        calls: list[str] = []
        with tempfile.TemporaryDirectory() as directory, ExitStack() as stack:
            root = Path(directory).resolve()
            inputs = root / "inputs"
            inputs.mkdir(mode=0o700)
            for index in range(len(guest.MATERIALS)):
                guest.write_file(inputs / str(index), ("original-" + str(index)).encode())
            originals: list[guest.Original] = []
            # Only the test's UID admission differs: retain actual originals/stamps/checks/closes.
            for index in range(len(guest.MATERIALS)):
                path = inputs / str(index)
                parent = os.open(inputs, os.O_RDONLY | os.O_DIRECTORY)
                _ = stack.callback(os.close, parent)
                descriptor = os.open(path, os.O_RDONLY)
                _ = stack.callback(os.close, descriptor)
                descriptors.extend((parent, descriptor))
                originals.append(
                    guest.Original(
                        parent,
                        descriptor,
                        path,
                        os.fstat(descriptor),
                        os.fstat(parent),
                        os.pread(descriptor, guest.CHUNK, 0),
                    )
                )
            control = guest.Control(time.monotonic(), 900, 60)
            commands = guest.Commands(root, control)
            bridge, _, _ = terminal_fixture()

            with (
                patch.object(guest.Commands, "call", side_effect=terminal_daemon(calls)),
                patch.object(os, "chown"),
                patch.object(
                    caddy_materials, "inspect", side_effect=caddy_materials.MaterialError()
                ),
                patch.object(
                    ssl, "create_default_context", side_effect=AssertionError("unexpected TLS")
                ),
            ):
                rows = guest.negative_copies(commands, originals, bridge, transport_report())
            known = list(commands.owned)
            for original in originals:
                original.check(control)
        closed = [descriptor_closed(descriptor) for descriptor in descriptors]
        self.assertTrue(all(closed))
        self.assertEqual(known, [])
        self.assertEqual(
            calls,
            [
                "negative-create",
                "negative-start",
                "negative-inspect",
                "negative-network",
                "negative-stop",
                "remove",
            ]
            * 2,
        )
        self.assertEqual([row["outcome"] for row in rows], ["refused-or-not-ready"] * 2)
        self.assertEqual([row["observedExitStatus"] for row in rows], [1, 1])
        self.assertEqual([row["tlsAttempted"] for row in rows], [False, False])

    def test_ssl_refusal_with_secondary_close_error_never_counts_or_retries(self) -> None:
        """Keep the same first SSL object while every actual original retires before assertions."""
        settled = transport_exchange("ssl-refusal", consumer="negative")
        self.assertIsNone(settled["failure"])
        for consumer in ("negative", "wait"):
            with self.subTest(consumer=consumer):
                value = transport_exchange("ssl-close", consumer=consumer)
                self.assertIs(value["failure"], value["expectedPrimary"])
                self.assertTrue(value["rawClosed"])
                self.assertTrue(value["wrappedClosed"])
                self.assertEqual(len(cast("list[object]", value["connect"])), 1)
                report = cast("dict[str, object]", value["report"])
                self.assertEqual(report["tlsFailureCategory"], "certificate-verification")


class CaddyNegativePhaseTests(unittest.TestCase):
    """Keep only actually clean phase-live refusals eligible for later terminal observation."""

    def test_running_copy_may_exit_before_its_fresh_negative_boundary_only_with_exact_joins(
        self,
    ) -> None:
        """Join initial running and final typed-exit observations under one original clock."""
        value = copy_phase_exchange("terminal")
        self.assertIsNone(value["failure"])
        self.assertEqual(value["copyOutcome"], ("refused-or-not-ready", guest.Refusal("exited", 1)))
        self.assertTrue(value["rawClosed"])
        self.assertTrue(value["wrappedClosed"])
        self.assertEqual(len(cast("list[object]", value["connect"])), 1)
        self.assertEqual(
            value["queries"],
            [
                ["docker", "inspect", CID],
                ["docker", "network", "inspect", NETWORK],
            ]
            * 2,
        )
        backoff = copy_phase_exchange("backoff")
        self.assertIsNone(backoff["failure"])
        self.assertEqual(
            backoff["copyOutcome"], ("refused-or-not-ready", guest.Refusal("restarting", 1))
        )
        self.assertEqual(backoff["queries"], value["queries"])
        for case in (
            "wrong-cid",
            "wrong-image",
            "wrong-network",
            "global-expiry",
            "signal",
            "successful-tls",
            "late-close",
            "failed-close",
        ):
            with self.subTest(case=case):
                refused = copy_phase_exchange(case)
                self.assertIsNotNone(refused["failure"])
                self.assertIsNone(refused["copyOutcome"])
                self.assertTrue(refused["rawClosed"])
                self.assertTrue(refused["wrappedClosed"])
                if case in {"late-close", "failed-close"}:
                    self.assertIs(refused["failure"], refused["expectedPrimary"])
                    self.assertEqual(len(cast("list[object]", refused["queries"])), 2)

    def test_ssl_refusal_never_qualifies_when_successful_close_completes_after_phase_end(
        self,
    ) -> None:
        """Keep the first SSL object while refusing every late-close negative observation."""
        for consumer in ("negative", "wait"):
            with self.subTest(consumer=consumer):
                value = transport_exchange("ssl-late-close", consumer=consumer)
                self.assertIs(value["failure"], value["expectedPrimary"])
                self.assertTrue(value["rawClosed"])
                self.assertTrue(value["wrappedClosed"])
                self.assertEqual(len(cast("list[object]", value["connect"])), 1)


class CaddyRestartObservationTests(unittest.TestCase):
    """Bind versioned engine backoff flags without calling them terminal or serving authority."""

    def test_exact_owned_restart_backoff_is_refusal_but_never_a_live_tls_target(self) -> None:
        """Observe Moby-shaped Running/Restarting true and Pid0 with a nonzero exit."""
        control = guest.Control(time.monotonic(), 900, 60)
        commands = guest.Commands(Path("/unused"), control, owned=[CID])
        mutations: tuple[tuple[str, object], ...] = (
            ("accepted", None),
            ("ExitCode", 0),
            ("ExitCode", True),
            ("ExitCode", None),
            ("Pid", 123),
            ("Pid", False),
            ("Pid", None),
            ("Running", False),
            ("Restarting", False),
            ("Status", "running"),
            ("Dead", True),
            ("Paused", True),
            ("OOMKilled", True),
        )
        for key, value in mutations:
            with self.subTest(key=key, value=value):
                bridge, row, network = backoff_fixture()
                if key != "accepted":
                    guest.obj(row["State"])[key] = value
                with patch.object(
                    guest.Commands,
                    "call",
                    side_effect=[
                        json.dumps([row]).encode(),
                        json.dumps([network]).encode(),
                    ],
                ):
                    if key == "accepted":
                        self.assertEqual(
                            guest.negative_endpoint(commands, CID, bridge),
                            (None, guest.Refusal("restarting", 1)),
                        )
                    else:
                        with self.assertRaises(guest.PrototypeError):
                            _ = guest.negative_endpoint(commands, CID, bridge)
                with self.assertRaises(guest.PrototypeError):
                    _ = guest.endpoint_observation(CID, bridge, row, network)

    def test_live_endpoint_requires_unpaused_running_state_with_positive_typed_pid(self) -> None:
        """A Running flag alone cannot authorize a connection to backoff, paused or dead state."""
        for key, value in (
            ("Pid", 0),
            ("Pid", True),
            ("Pid", -1),
            ("Pid", None),
            ("Status", "restarting"),
            ("Restarting", True),
            ("Paused", True),
            ("Dead", True),
        ):
            with self.subTest(key=key, value=value):
                bridge, row, network = transport_fixture()
                guest.obj(row["State"])[key] = value
                with self.assertRaises(guest.PrototypeError):
                    _ = guest.endpoint_observation(CID, bridge, row, network)


if __name__ == "__main__":
    _ = unittest.main()
