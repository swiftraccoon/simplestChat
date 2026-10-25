"""Exercise prototype custody and containment decisions using only owned local files/fakes."""

from __future__ import annotations

import errno
import io
import json
import os
import socket
import ssl
import tarfile
import tempfile
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import TYPE_CHECKING, BinaryIO, cast, override
from unittest.mock import patch

from test_support import ROOT

# isort: split
import bounded_process
import caddy_probe_guest as guest

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping, Sequence

CID = "a" * 64


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
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        with (
            patch.object(guest, "tls_probe", side_effect=guest.PrototypeError("guest_deadline")),
            self.assertRaisesRegex(guest.PrototypeError, "^guest_deadline$"),
        ):
            guest.negative_tls(control, context, guest.DOMAIN)

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


if __name__ == "__main__":
    _ = unittest.main()
