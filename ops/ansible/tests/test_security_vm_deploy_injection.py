"""Exercise the fixed private inspection barrier without Docker, mounts or a VM."""

from __future__ import annotations

import hashlib
import io
import json
import os
import socket
import struct
import sys
import tempfile
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import TYPE_CHECKING, cast
from unittest.mock import Mock, patch

from test_support import ROOT

# isort: split
import release_container_harness as fixture
import security_vm_deploy_guest as guest
import security_vm_deploy_injection as injection
import security_vm_ops_guest as ops_guest
import security_vm_stage_guest as stage

if TYPE_CHECKING:
    from typing import BinaryIO

IDENTIFIER = "a" * 64
OLD = "b" * 64
TOKEN = "c" * 32
IMAGE = "sha256:" + "d" * 64
FSTAT = os.fstat
LSTAT = Path.lstat


def mapped(value: os.stat_result) -> os.stat_result:
    """Map only fixture UID/GID observations; retain real file identity and bytes."""
    fields = list(value)
    fields[4] = fields[5] = 0
    return os.stat_result(fields)


def root_fstat(descriptor: int) -> os.stat_result:
    """Keep the real retained inode while mapping only the fixture owner observation."""
    return mapped(FSTAT(descriptor))


def root_lstat(path: Path) -> os.stat_result:
    """Keep the real namespace identity while mapping only fixture ownership."""
    return mapped(LSTAT(path))


def private_empty(path: Path) -> Path:
    """Create a fixed empty private fixture original without a permissive creation window."""
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    os.close(descriptor)
    return path


def empty_data(_maximum: int) -> bytes:
    """Supply an explicit absent-stderr observation for the lower effect-fault fixture."""
    return b""


class InspectionBarrierTests(unittest.TestCase):
    """Keep the observed effect, response and checked receipt boundaries distinct."""

    def test_exact_native_inspection_match_does_not_capture_other_docker_roles(self) -> None:
        """Only the complete reviewed format and one full CID can enter the bridge."""
        arguments = [
            "--host",
            "unix:///var/run/docker.sock",
            "inspect",
            "--format",
            injection.NATIVE_FORMAT,
            IDENTIFIER,
        ]
        self.assertEqual(injection.matches(arguments), IDENTIFIER)
        for changed in (
            arguments[:-1],
            [*arguments, "extra"],
            [*arguments[:-1], "name"],
            [*arguments[:4], '{"labels":{{json .Config.Labels}}}', IDENTIFIER],
            ["--host", "tcp://127.0.0.1:2375", *arguments[2:]],
        ):
            self.assertIsNone(injection.matches(changed))

    def test_real_response_is_forwarded_only_after_owned_stop_and_sealed_receipt(self) -> None:
        """Real private files and sockets preserve raw bytes after the fixed effect boundary."""
        trace: list[str] = []
        observed: dict[str, object] = {}
        streams: list[BinaryIO] = []
        writers: list[BinaryIO] = []
        adopt = stage.adopt
        with tempfile.TemporaryDirectory(prefix="di-", dir="/tmp") as temporary:
            root = Path(temporary)
            control = guest.Control(time.monotonic())
            control.phase = "exercise"
            commands = guest.Commands(
                control,
                cast("guest.Wire", cast("object", SimpleNamespace(no_extra=lambda: None))),
                [],
            )
            response = (
                json.dumps(
                    {
                        "id": IDENTIFIER,
                        "image": IMAGE,
                        "state": {
                            "Running": True,
                            "OOMKilled": False,
                            "StartedAt": "earlier",
                            "FinishedAt": "0001-01-01T00:00:00Z",
                        },
                        "restarts": 0,
                        "configHash": "f" * 64,
                    }
                ).encode()
                + b"\n"
            )
            error_path = private_empty(root / "actual-inspect.stderr")
            before: dict[str, object] = {
                "id": IDENTIFIER,
                "image": IMAGE,
                "labels": {
                    "com.docker.compose.service": "simplestchat",
                },
                "state": {
                    "Running": True,
                    "StartedAt": "earlier",
                    "FinishedAt": "0001-01-01T00:00:00Z",
                },
            }
            after: dict[str, object] = {
                "id": IDENTIFIER,
                "image": IMAGE,
                "labels": before["labels"],
                "state": {"Running": False, "FinishedAt": "2026-10-25T01:00:00Z"},
            }

            def inspect(*_arguments: object) -> dict[str, object]:
                trace.append("inspect-owned-after" if "stop" in trace else "inspect-owned-before")
                return after if "stop" in trace else before

            def docker(*arguments: str, **_options: object) -> guest.Command:
                trace.append(arguments[0])
                return guest.Command(0, response if arguments[0] == "inspect" else b"", error_path)

            def compose(*_arguments: str, **_options: object) -> guest.Command:
                trace.append("compose")
                return guest.Command(0, (IDENTIFIER + "\n").encode(), error_path)

            def admitted(descriptor: int, mode: str) -> BinaryIO:
                stream = adopt(descriptor, mode)
                streams.append(stream)
                if mode == "wb":
                    writers.append(stream)
                return stream

            with (
                patch.object(injection, "SOCKET", root / "socket"),
                patch.object(injection, "RECEIPT", root / "receipt.json"),
                patch.object(guest, "PUBLIC", root),
                patch.object(guest, "active_attempt", return_value=root / "release.fixed"),
                patch.object(
                    guest,
                    "read_record",
                    return_value={
                        "schemaVersion": 1,
                        "attempt": str(root / "release.fixed"),
                        "phase": "replace_application",
                        "finalized": False,
                    },
                ),
                patch.object(fixture, "inspect_owned", side_effect=inspect),
                patch.object(commands, "compose", side_effect=compose),
                patch.object(commands, "docker", side_effect=docker),
                patch.object(os, "fstat", side_effect=root_fstat),
                patch.object(Path, "lstat", autospec=True, side_effect=root_lstat),
                patch.object(stage, "adopt", side_effect=admitted),
            ):
                owner = guest.Injector(
                    commands,
                    guest.InjectionBinding(
                        IMAGE,
                        OLD,
                        TOKEN,
                        frozenset({OLD}),
                        frozenset(),
                    ),
                )
                left, right = socket.socketpair()
                left.settimeout(0)
                owner.client = left
                owner.service_end = time.monotonic() + guest.INJECTION_SERVICE_SECONDS
                caught: BaseException | None = None
                try:
                    digest = owner.deliver(IDENTIFIER)
                    frame = injection.receive_json(right, time.monotonic() + 2)
                    raw = injection.read_exact(right, len(response), time.monotonic() + 2)
                    receipt = guest.record((root / "receipt.json").read_bytes())
                    observed.update(
                        {
                            "digest": digest,
                            "frame": frame,
                            "raw": raw,
                            "receipt": receipt,
                            "writerClosed": len(writers) == 1 and writers[0].closed,
                            "stopped": owner.stopped,
                            "identifier": owner.identifier,
                        }
                    )
                except BaseException as error:  # noqa: BLE001 -- Close peers before asserting.
                    caught = error
                finally:
                    first = ops_guest.settle((owner.close, right.close), caught)
                    first = ops_guest.settle(
                        tuple(item.close for item in commands.originals), first
                    )
                    observed["allOriginalsClosed"] = all(item.closed for item in commands.originals)
            self.assertIsNone(first)
            self.assertTrue(observed["allOriginalsClosed"])
        self.assert_delivered(observed, response, trace)

    def assert_delivered(
        self,
        observed: dict[str, object],
        response: bytes,
        trace: list[str],
    ) -> None:
        """Assert independent raw/effect facts only after every actual owner is closed."""
        self.assertEqual(
            trace, ["compose", "inspect-owned-before", "inspect", "stop", "inspect-owned-after"]
        )
        self.assertEqual(observed["raw"], response)
        self.assertTrue(observed["writerClosed"] is True)
        self.assertTrue(observed["stopped"] is True)
        self.assertEqual(observed["identifier"], IDENTIFIER)
        receipt = cast("dict[str, object]", observed["receipt"])
        self.assertEqual(receipt["stdoutSha256"], hashlib.sha256(response).hexdigest())
        self.assertEqual(receipt["responseRawExit"], 0)
        self.assertEqual(
            cast("dict[str, object]", observed["frame"])["receiptSha256"], observed["digest"]
        )

    def test_foreign_candidate_or_journal_never_admits_a_stop(self) -> None:
        """Closed identity joins, rather than the request alone, authorize the one effect."""
        outcomes: list[tuple[bool, str | None, int]] = []
        for kind in ("journal", "identifier", "image", "service"):
            with (
                self.subTest(kind=kind),
                tempfile.TemporaryDirectory(
                    prefix="di-",
                    dir="/tmp",
                ) as temporary,
            ):
                root = Path(temporary)
                control = guest.Control(time.monotonic())
                commands = guest.Commands(
                    control,
                    cast("guest.Wire", cast("object", SimpleNamespace(no_extra=lambda: None))),
                    [],
                )
                selected = IDENTIFIER if kind != "identifier" else "e" * 64
                journal = {
                    "schemaVersion": 1,
                    "attempt": str(root / "attempt"),
                    "phase": "complete" if kind == "journal" else "replace_application",
                    "finalized": False,
                }
                owned = {
                    "id": IDENTIFIER,
                    "image": IMAGE if kind != "image" else "sha256:" + OLD,
                    "labels": {
                        "com.docker.compose.service": "postgres"
                        if kind == "service"
                        else "simplestchat"
                    },
                }
                with (
                    patch.object(injection, "SOCKET", root / "socket"),
                    patch.object(guest, "active_attempt", return_value=root / "attempt"),
                    patch.object(guest, "read_record", return_value=journal),
                    patch.object(fixture, "inspect_owned", return_value=owned),
                    patch.object(
                        commands,
                        "compose",
                        return_value=guest.Command(
                            0,
                            selected.encode(),
                            root / "unused",
                        ),
                    ),
                    patch.object(commands, "docker") as docker,
                ):
                    owner = guest.Injector(
                        commands,
                        guest.InjectionBinding(
                            IMAGE,
                            OLD,
                            TOKEN,
                            frozenset({OLD}),
                            frozenset(),
                        ),
                    )
                    left, right = socket.socketpair()
                    left.settimeout(0)
                    owner.client = left
                    owner.service_end = time.monotonic() + guest.INJECTION_SERVICE_SECONDS
                    caught: BaseException | None = None
                    try:
                        _ = owner.deliver(IDENTIFIER)
                    except ValueError as error:
                        caught = error
                    finally:
                        first = ops_guest.settle((owner.close, right.close), caught)
                    outcomes.append(
                        (isinstance(first, ValueError), owner.identifier, docker.call_count)
                    )
        self.assertEqual(outcomes, [(True, None, 0)] * 4)

    def test_partial_or_wrong_nonce_request_closes_the_real_local_peer(self) -> None:
        """A private endpoint is still parsed as hostile fixed data, never as command authority."""
        outcomes: list[tuple[bool, bytes, str | None]] = []
        for kind in ("partial", "nonce", "extra"):
            with (
                self.subTest(kind=kind),
                tempfile.TemporaryDirectory(
                    prefix="di-",
                    dir="/tmp",
                ) as temporary,
            ):
                root = Path(temporary)
                control = guest.Control(time.monotonic())
                commands = guest.Commands(
                    control,
                    cast("guest.Wire", cast("object", SimpleNamespace(no_extra=lambda: None))),
                    [],
                )
                with patch.object(injection, "SOCKET", root / "socket"):
                    owner = guest.Injector(
                        commands,
                        guest.InjectionBinding(
                            IMAGE,
                            OLD,
                            TOKEN,
                            frozenset({OLD}),
                            frozenset(),
                        ),
                    )
                    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                    client.settimeout(2)
                    client.connect(str(root / "socket"))
                    request: dict[str, object] = {
                        "schemaVersion": 1,
                        "nonce": "e" * 32,
                        "identifier": IDENTIFIER,
                    }
                    if kind == "extra":
                        request["secret-field"] = "never-public"
                    if kind == "partial":
                        client.sendall(b"\x00\x00")
                        client.shutdown(socket.SHUT_WR)
                    else:
                        injection.send_json(client, request, time.monotonic() + 2)
                    owner.leader_pid = 1234
                    caught: BaseException | None = None
                    peer = b"not-closed"
                    try:
                        with (
                            patch.object(
                                socket.socket,
                                "getsockopt",
                                return_value=struct.pack("3i", 1234, 0, 0),
                            ),
                            patch.object(owner, "peer_parent", return_value=1234),
                        ):
                            owner.poll()
                    except (ValueError, OSError) as error:
                        caught = error
                    finally:
                        peer = client.recv(1)
                        first = ops_guest.settle((owner.close, client.close), caught)
                    outcomes.append(
                        (isinstance(first, (ValueError, OSError)), peer, owner.identifier)
                    )
        self.assertEqual(outcomes, [(True, b"", None)] * 3)

    def test_known_candidate_survives_stop_or_receipt_failure_without_forwarding(self) -> None:
        """An observed exact actor remains owned, but failed effects never release raw success."""
        facts: list[tuple[object, ...]] = []
        zero, later = "0001-01-01T00:00:00Z", "2026-10-25T01:00:00Z"
        cases = {
            "stop": (zero, later),
            "receipt": (zero, later),
            "missing-before": (None, later),
            "missing-after": (zero, None),
            "unchanged": (zero, zero),
            "zero-after": (later, zero),
        }
        for role, (before_finished, after_finished) in cases.items():
            with (
                self.subTest(role=role),
                tempfile.TemporaryDirectory(
                    prefix="di-",
                    dir="/tmp",
                ) as temporary,
            ):
                root = Path(temporary)
                primary = OSError("fixed_body")
                secondary = OSError("fixed_close")
                control = guest.Control(time.monotonic())
                commands = guest.Commands(
                    control,
                    cast("guest.Wire", cast("object", SimpleNamespace(no_extra=lambda: None))),
                    [],
                )
                value = {
                    "id": IDENTIFIER,
                    "image": IMAGE,
                    "labels": {"com.docker.compose.service": "simplestchat"},
                    "state": {
                        "Running": True,
                        "OOMKilled": False,
                        "StartedAt": "before",
                        **({"FinishedAt": before_finished} if before_finished is not None else {}),
                    },
                }
                response = json.dumps(
                    {
                        "id": IDENTIFIER,
                        "image": IMAGE,
                        "state": value["state"],
                        "restarts": 0,
                        "configHash": "f" * 64,
                    }
                ).encode()
                after = {
                    **value,
                    "state": {
                        "Running": False,
                        **({"FinishedAt": after_finished} if after_finished is not None else {}),
                    },
                }

                binding = (role, primary, response, root)

                def docker(
                    *args: str,
                    binding: tuple[str, OSError, bytes, Path] = binding,
                    **_options: object,
                ) -> guest.Command:
                    role_name, cause, raw, directory = binding
                    if args[0] == "stop" and role_name == "stop":
                        raise cause
                    return guest.Command(0, raw, directory / "unused")

                with (
                    patch.object(injection, "SOCKET", root / "socket"),
                    patch.object(guest, "active_attempt", return_value=root / "attempt"),
                    patch.object(
                        guest,
                        "read_record",
                        return_value={
                            "schemaVersion": 1,
                            "attempt": str(root / "attempt"),
                            "phase": "replace_application",
                            "finalized": False,
                        },
                    ),
                    patch.object(fixture, "inspect_owned", side_effect=[value, after]),
                    patch.object(
                        commands,
                        "compose",
                        return_value=guest.Command(
                            0,
                            IDENTIFIER.encode(),
                            root / "unused",
                        ),
                    ),
                    patch.object(commands, "docker", side_effect=docker) as docker_calls,
                    patch.object(guest, "retain", return_value=SimpleNamespace(data=empty_data)),
                ):
                    owner = guest.Injector(
                        commands,
                        guest.InjectionBinding(
                            IMAGE,
                            OLD,
                            TOKEN,
                            frozenset({OLD}),
                            frozenset(),
                        ),
                    )
                    left, right = socket.socketpair()
                    right.settimeout(0)

                    def closing(
                        stream: socket.socket = left,
                        error: OSError = secondary,
                    ) -> None:
                        stream.close()
                        raise error

                    owner.client = cast(
                        "socket.socket",
                        cast(
                            "object",
                            SimpleNamespace(
                                recv=left.recv,
                                settimeout=left.settimeout,
                                close=closing,
                            ),
                        ),
                    )
                    owner.service_end = time.monotonic() + guest.INJECTION_SERVICE_SECONDS
                    caught: BaseException | None = None
                    writes = Mock(side_effect=primary)
                    try:
                        with patch.object(owner, "write_owned", new=writes):
                            _ = owner.deliver(IDENTIFIER)
                    except (OSError, ValueError) as error:
                        caught = error
                    forwarded: list[bytes] = []

                    def sample(
                        peer: socket.socket = right,
                        rows: list[bytes] = forwarded,
                    ) -> None:
                        rows.append(peer.recv(1))

                    first = ops_guest.settle((owner.close, sample, right.close), caught)
                    facts.append(
                        (
                            first is caught,
                            "body" if first is primary else type(first).__name__,
                            owner.identifier,
                            IDENTIFIER in commands.known,
                            left.fileno() == right.fileno() == -1,
                            forwarded == [b""],
                            writes.call_count,
                            sum(call.args[0] == "stop" for call in docker_calls.call_args_list),
                        )
                    )
        self.assertEqual(
            facts,
            [
                (True, "body", IDENTIFIER, True, True, True, 0, 1),
                (True, "body", IDENTIFIER, True, True, True, 1, 1),
                (True, "ValueError", None, False, True, True, 0, 0),
                *[(True, "ValueError", IDENTIFIER, True, True, True, 0, 1)] * 3,
            ],
        )

    def test_actual_receipt_body_and_close_fault_keep_the_first_error(self) -> None:
        """An admitted real file closes despite paired sync/close failure; no reply is earned."""
        primary = OSError("fixed_sync")
        secondary = OSError("fixed_close")
        adopt = stage.adopt
        streams: list[BinaryIO] = []
        caught: BaseException | None = None
        with tempfile.TemporaryDirectory(prefix="di-", dir="/tmp") as temporary:
            root = Path(temporary)
            control = guest.Control(time.monotonic())
            commands = guest.Commands(
                control,
                cast("guest.Wire", cast("object", SimpleNamespace(no_extra=lambda: None))),
                [],
            )

            def admitted(descriptor: int, mode: str) -> BinaryIO:
                original = adopt(descriptor, mode)
                streams.append(original)

                def closing() -> None:
                    original.close()
                    raise secondary

                return cast(
                    "BinaryIO",
                    cast(
                        "object",
                        SimpleNamespace(
                            write=original.write,
                            flush=original.flush,
                            fileno=original.fileno,
                            close=closing,
                        ),
                    ),
                )

            with (
                patch.object(injection, "SOCKET", root / "socket"),
                patch.object(os, "fstat", side_effect=root_fstat),
                patch.object(Path, "lstat", autospec=True, side_effect=root_lstat),
            ):
                owner = guest.Injector(
                    commands,
                    guest.InjectionBinding(
                        IMAGE,
                        OLD,
                        TOKEN,
                        frozenset({OLD}),
                        frozenset(),
                    ),
                )
                try:
                    with (
                        patch.object(stage, "adopt", side_effect=admitted),
                        patch.object(
                            os,
                            "fsync",
                            side_effect=primary,
                        ),
                    ):
                        _ = owner.write_owned(root / "receipt.json", b"{}\n")
                except OSError as error:
                    caught = error
                finally:
                    first = ops_guest.settle((owner.close,), caught)
            closed = [stream.closed for stream in streams]
        self.assertIs(first, primary)
        self.assertEqual(closed, [True])
        self.assertEqual(commands.known, set())

    def test_exact_peer_closure_and_expired_service_never_authorize_a_reply(self) -> None:
        """Actual local peers and the original deadline remain negative observations."""
        failures: list[bool] = []
        with tempfile.TemporaryDirectory(prefix="di-", dir="/tmp") as temporary:
            root = Path(temporary)
            for expired in (False, True):
                control = guest.Control(time.monotonic())
                commands = guest.Commands(
                    control,
                    cast("guest.Wire", cast("object", SimpleNamespace(no_extra=lambda: None))),
                    [],
                )
                with patch.object(injection, "SOCKET", root / "socket"):
                    owner = guest.Injector(
                        commands,
                        guest.InjectionBinding(
                            IMAGE,
                            OLD,
                            TOKEN,
                            frozenset({OLD}),
                            frozenset(),
                        ),
                    )
                    left, right = socket.socketpair()
                    left.settimeout(0)
                    owner.client = left
                    owner.service_end = time.monotonic() + (-1 if expired else 20)
                    right.close()
                    caught = False
                    try:
                        owner.check_active()
                    except ValueError:
                        caught = True
                    finally:
                        owner.close()
                    failures.append(caught)
                    self.assertIsNone(owner.identifier)
        self.assertEqual(failures, [True, True])

    def test_original_body_expiry_closes_all_sockets_without_lending_finalization_to_unlink(
        self,
    ) -> None:
        """A real finalization window does not authorize new normal namespace mutation."""
        facts: list[tuple[bool, bool, bool, bool]] = []
        for prior in (None, OSError("fixed_prior")):
            with tempfile.TemporaryDirectory(prefix="di-", dir="/tmp") as temporary:
                path = Path(temporary) / "socket"
                now = [100.0]
                control = guest.Control(now[0])
                control.phase = "exercise"
                commands = guest.Commands(
                    control,
                    cast("guest.Wire", cast("object", SimpleNamespace(no_extra=lambda: None))),
                    [],
                )

                def observed_time(point: list[float] = now) -> float:
                    return point[0]

                with (
                    patch.object(injection, "SOCKET", path),
                    patch.object(
                        time,
                        "monotonic",
                        side_effect=observed_time,
                    ),
                ):
                    owner = guest.Injector(
                        commands,
                        guest.InjectionBinding(
                            IMAGE,
                            OLD,
                            TOKEN,
                            frozenset({OLD}),
                            frozenset(),
                        ),
                    )
                    left, right = socket.socketpair()
                    owner.client = left
                    now[0] = control.body
                    first = ops_guest.settle((owner.close, right.close), prior)
                    facts.append(
                        (
                            first is prior if prior is not None else isinstance(first, ValueError),
                            left.fileno() == right.fileno() == -1,
                            owner.server is not None and owner.server.fileno() == -1,
                            path.exists(),
                        )
                    )
        self.assertEqual(facts, [(True, True, True, True)] * 2)

    def test_context_expiry_or_signal_refuses_before_any_new_original(self) -> None:
        """Only initial fixed config bootstrap is unclocked; later opens cannot borrow time."""
        observed: list[tuple[bool, int, int]] = []
        for number, deadline in ((0, 0.0), (2, time.monotonic() + 20)):
            context = injection.Context.__new__(injection.Context)
            context.end = deadline
            context.originals = []
            with (
                patch.object(injection, "_observed_signal", number),
                patch.object(
                    injection,
                    "Original",
                ) as constructor,
            ):
                caught = False
                try:
                    _ = context.open(Path("/never-admitted"), 10, 0o600)
                except ValueError:
                    caught = True
                observed.append((caught, constructor.call_count, len(context.originals)))
        self.assertEqual(observed, [(True, 0, 0), (True, 0, 0)])

    def test_context_late_open_is_registered_and_closed_without_replacing_deadline(self) -> None:
        """A real admitted original survives a late fence and paired checked-close error."""
        original_type, actual_close = injection.Original, os.close
        now = [100.0]
        primary: BaseException | None = None
        secondary = OSError("fixed_close")
        context = injection.Context.__new__(injection.Context)
        context.end, context.originals, context.inherited = 101.0, [], []
        with tempfile.TemporaryDirectory(prefix="di-", dir="/tmp") as temporary:
            path = private_empty(Path(temporary).resolve() / "late-original")

            def late(
                path: Path, maximum: int, mode: int, *, empty: bool = False
            ) -> injection.Original:
                original = original_type(path, maximum, mode, empty=empty)
                now[0] = 102.0
                return original

            def closing(descriptor: int) -> None:
                actual_close(descriptor)
                raise secondary

            def observed_time(point: list[float] = now) -> float:
                return point[0]

            with (
                patch.object(os, "fstat", side_effect=root_fstat),
                patch.object(Path, "lstat", autospec=True, side_effect=root_lstat),
                patch.object(time, "monotonic", side_effect=observed_time),
                patch.object(injection, "Original", side_effect=late),
            ):
                try:
                    _ = context.open(path, 10, 0o600, empty=True)
                except ValueError as error:
                    primary = error
                with patch.object(os, "close", side_effect=closing):
                    first = ops_guest.settle((context.close,), primary)
            closed = [original.fd is None for original in context.originals]
        self.assertIsInstance(primary, ValueError)
        self.assertIs(first, primary)
        self.assertEqual(closed, [True])

    def test_bootstrap_refusal_checked_closes_the_known_inherited_descriptor(self) -> None:
        """A refusal before project imports still attempts the admitted entry close."""
        caught: object = None
        evidence = io.BytesIO()
        with tempfile.TemporaryFile() as original:
            descriptor = os.dup(original.fileno())
            admitted = [descriptor]
            with (
                patch.object(injection, "_entry_descriptors", admitted),
                patch.object(
                    sys,
                    "stderr",
                    SimpleNamespace(buffer=evidence),
                ),
            ):
                try:
                    injection.refuse_bootstrap()
                except SystemExit as error:
                    caught = error.code
            closed = False
            try:
                _ = os.fstat(descriptor)
            except OSError:
                closed = True
        self.assertEqual(caught, 1)
        self.assertEqual(admitted, [])
        self.assertTrue(closed)
        self.assertEqual(evidence.getvalue(), injection.FAILURE)

    def test_helper_final_fence_refuses_a_late_signal_or_original_clock(self) -> None:
        """No prior delivered flag overrides cancellation or expiry after explicit close/restore."""
        evidence = io.BytesIO()
        observed: list[int] = []
        with patch.object(sys, "stderr", SimpleNamespace(buffer=evidence)):
            for number, end in ((0, time.monotonic() + 20), (2, time.monotonic() + 20), (0, 0)):
                with patch.object(injection, "_observed_signal", number):
                    observed.append(
                        injection.status_after_close(
                            passed=True,
                            first=None,
                            deadline=end,
                        )
                    )
        self.assertEqual(observed, [0, 1, 1])
        self.assertEqual(evidence.getvalue(), injection.FAILURE * 2)

    def test_duplicate_or_oversize_framed_json_refuses_after_actual_socket_close(self) -> None:
        """The mature unique-key parser and fixed byte cap apply before command authority."""
        outcomes: list[bool] = []
        for raw in (b'{"nonce":"a","nonce":"b"}', b"x" * (injection.FRAME + 1)):
            left, right = socket.socketpair()
            left.settimeout(2)
            right.settimeout(2)
            caught = False
            try:
                prefix = struct.pack("!I", len(raw))
                left.sendall(prefix if len(raw) > injection.FRAME else prefix + raw)
                try:
                    _ = injection.receive_json(right, time.monotonic() + 2)
                except ValueError:
                    caught = True
            finally:
                first = ops_guest.settle((left.close, right.close))
            outcomes.append(caught and first is None and left.fileno() == right.fileno() == -1)
        self.assertEqual(outcomes, [True, True])

    def test_docker_exec_environment_preserves_the_actual_native_source_profile(self) -> None:
        """The helper must not add HOME or ambient credentials to a NativeIO Docker role."""
        source = (ROOT / "ops/src/release/attestation/deploy/process.rs").read_text()
        self.assertEqual(
            injection.DOCKER_ENV, {"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LC_ALL": "C"}
        )
        self.assertNotIn("HOME", injection.DOCKER_ENV)
        self.assertIn('("PATH".into(), "/usr/sbin:/usr/bin:/sbin:/bin".into())', source)
        self.assertIn('("LC_ALL".into(), "C".into())', source)

    def test_source_placement_keeps_positive_outside_the_negative_namespace(self) -> None:
        """The fixed transfer and decoder name the qualified negative profile only."""
        source = (ROOT / "build/security_vm_deploy_guest.py").read_text()
        playbook = (ROOT / "ops/ansible/security-vm-deploy.yml").read_text()
        self.assertIn("if negative:", source)
        self.assertIn('"negative-inspection-response-barrier-v1"', source)
        self.assertIn('"original-docker-cli"', source)
        self.assertIn('"--propagation"', source)
        self.assertIn("security_vm_deploy_injection.py", playbook)
        self.assertIn("mode: '0755'", playbook)


if __name__ == "__main__":
    _ = unittest.main()
