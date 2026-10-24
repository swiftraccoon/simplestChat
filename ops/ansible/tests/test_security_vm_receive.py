"""Inert receiver-keeper tests; no real home, guest, service or network is used."""

from __future__ import annotations

import hashlib
import http.client
import io
import os
import pwd
import selectors
import shlex
import signal
import sys
import tempfile
import time
import unittest
from pathlib import Path
from typing import TYPE_CHECKING, cast, final, override
from unittest.mock import patch

import test_release_attestation as prepare_reference
import test_release_fetch as fetch_data
import test_security_vm_fetch as fetch_reference
from test_support import ROOT, obj, string, yaml_value

sys.path.insert(0, str(ROOT / "build"))
import bounded_process
import release_attestation as attest
import release_fetch_controller as reference
import security_vm as vm
import security_vm_fetch as fetch_vm
import security_vm_ops as ops_vm
import security_vm_ops_guest as ops_guest
import security_vm_receive as keeper
import security_vm_receive_guest as guest
import security_vm_receive_route as route
from security_tools import ToolError

if TYPE_CHECKING:
    import socket
    from collections.abc import Callable, Mapping, Sequence
    from typing import BinaryIO, ClassVar

    from release_json import JsonObject, JsonValue

LATE_RESTORE_CALL = 3
REVISION = "a" * 40
CI = {
    "GITHUB_ACTIONS": "true",
    "RUNNER_ENVIRONMENT": "github-hosted",
    "GITHUB_REPOSITORY": fetch_vm.REPOSITORY,
    "GITHUB_REF": "refs/heads/main",
    "GITHUB_WORKFLOW": keeper.WORKFLOW,
    "GITHUB_SHA": REVISION,
    "GH_TOKEN": "INERT_RECEIVE_FIXTURE_TOKEN",
}


def write(path: Path, data: bytes, mode: int = 0o600) -> None:
    """Create only fresh owned fixture leaves."""
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    with path.open("xb") as output:
        _ = output.write(data)
    path.chmod(mode)


@final
class AuthenticationHost(keeper.Host):
    """Stop at a precise actual-command boundary without creating any guest."""

    events: ClassVar[list[str]] = []
    fail_server: ClassVar[bool] = False

    @override
    def command(
        self,
        name: str,
        argv: Sequence[str],
        environment: Mapping[str, str],
        timeout: float,
        accepted: tuple[int, ...],
    ) -> tuple[int, bytes]:
        """Require the current child verdict despite any saved observation."""
        self.events.append(name)
        if name == "authenticate-ops":
            write(self.output / "ops-acquired/acquisition-observation.json", b'{"passed":true}\n')
        _ = argv, environment, timeout, accepted
        if name == "authenticate-ops" and self.fail_server:
            names = [
                "simplestchat-ops",
                "ops-release.json",
                "ops-evidence.json",
                "ops-predicate.json",
                "ops-preparation.json",
                "ops-attestation.jsonl",
            ]
            names.extend(f"ops-evidence-{index:02d}.bin" for index in range(9))
            for item in names:
                write(self.output / "ops-acquired/input" / item, b"INERT_AUTH_PAYLOAD")
            return 0, b""
        if name == "authenticate-retained-ops" and self.fail_server:
            return 0, b""
        message = "PRIVATE_INERT_AUTH_FAILURE"
        raise ToolError(message)


class ReceiveKeeperTests(unittest.TestCase):
    """Bind policy refusals to actual files, owned children and finalizer facts."""

    def test_route_requires_exact_shape_and_integer_pins(self) -> None:
        """Verify route requires exact shape and integer pins."""
        value = {"schemaVersion": 1, "runId": "b" * 32, "binarySha256": "c" * 64, "binaryBytes": 20}
        self.assertEqual(route.config_value(keeper.encoded(value)), value)
        for change in (
            {"extra": 1},
            {"binaryBytes": True},
            {"runId": "x"},
            {"binarySha256": "URL_PRIVATE"},
            {"binaryBytes": 0},
        ):
            with self.subTest(change=change), self.assertRaises(ValueError):
                _ = route.config_value(keeper.encoded(dict(value, **change)))
        with self.assertRaises(ValueError):
            _ = route.config_value(b'{"schemaVersion":1,"schemaVersion":1}')

    def test_admission_refuses_developer_environment_before_home_lookup(self) -> None:
        """Verify admission refuses developer environment before home lookup."""
        with patch.object(pwd, "getpwuid") as home, self.assertRaises(ToolError):
            _ = keeper.admission({})
        home.assert_not_called()
        for key in (
            "GITHUB_ACTIONS",
            "RUNNER_ENVIRONMENT",
            "GITHUB_REPOSITORY",
            "GITHUB_REF",
            "GITHUB_WORKFLOW",
        ):
            with self.subTest(key=key), patch.object(pwd, "getpwuid") as home:
                with self.assertRaises(ToolError):
                    _ = keeper.admission(dict(CI, **{key: "wrong"}))
                home.assert_not_called()

    def test_route_root_refusal_does_not_read_config_or_execute(self) -> None:
        """Verify route root refusal does not read config or execute."""
        with (
            patch.object(os, "geteuid", return_value=123),
            patch.object(route, "read_original") as read,
            patch.object(os, "execve") as execute,
            self.assertRaises(ValueError),
        ):
            route.native()
        read.assert_not_called()
        execute.assert_not_called()

    def test_route_rejects_nonroot_owned_binary_and_closes_original(self) -> None:
        """Verify route rejects nonroot owned binary and closes original."""
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "binary"
            write(path, b"not executed", 0o755)
            value = {
                "schemaVersion": 1,
                "runId": "b" * 32,
                "binarySha256": hashlib.sha256(b"not executed").hexdigest(),
                "binaryBytes": len(b"not executed"),
            }
            opened: list[int] = []
            real_open = os.open

            def capture(*args: object, **kwargs: object) -> int:
                descriptor = real_open(cast("Path", args[0]), cast("int", args[1]))
                _ = kwargs
                opened.append(descriptor)
                return descriptor

            with (
                patch.object(route, "BINARY", path),
                patch.object(os, "geteuid", return_value=0),
                patch.object(
                    route, "read_original", side_effect=[keeper.encoded(value), b"b" * 32 + b"\n"]
                ),
                patch.object(os, "open", side_effect=capture),
                patch.object(os, "execve") as execute,
                self.assertRaises(ValueError),
            ):
                route.native()
            execute.assert_not_called()
            self.assertEqual(len(opened), 1)
            with self.assertRaises(OSError):
                _ = os.fstat(opened[0])

    def session(self, root: Path, body: str) -> keeper.Session:
        """Start only a source-owned inert interpreter child, never SSH/QEMU."""
        script = root / "peer.py"
        write(script, body.encode())
        return keeper.Session(
            [sys.executable, "-I", "-B", str(script)],
            {"PATH": "/usr/bin:/bin", "LC_ALL": "C"},
            fetch_vm.Control(),
        )

    def test_session_preserves_two_frames_and_open_stdin_until_final_record(self) -> None:
        """Verify session preserves two frames and open stdin until final record."""
        body = """import json,select,sys
first=json.loads(sys.stdin.buffer.readline())
assert first == {"envelope":1}
print('{"schemaVersion":1,"status":"ready"}',flush=True)
second=json.loads(sys.stdin.buffer.readline())
assert second == {"url":"INERT_PRIVATE_TARGET"}
assert not select.select([sys.stdin],[],[],0.03)[0]
print('{"passed":false,"phase":"download"}',flush=True)
raise SystemExit(1)
"""
        with tempfile.TemporaryDirectory() as directory:
            session = self.session(Path(directory), body)
            try:
                session.send({"envelope": 1})
                ready = session.line(5)
                session.send({"url": "INERT_PRIVATE_TARGET"})
                receipt = session.line(5)
                status = session.finish()
            finally:
                session.close()
            self.assertEqual(status, 1)
            self.assertEqual(ready, {"schemaVersion": 1, "status": "ready"})
            self.assertEqual(receipt, {"passed": False, "phase": "download"})
            self.assertTrue(all(stream.closed for stream in session.streams))
            self.assertEqual(session.eof, {"stdout", "stderr"})
            self.assertIsNotNone(session.child)
            if session.child is not None:
                self.assertEqual(session.child.returncode, 1)

    def test_session_missing_record_refuses_and_settles_every_peer(self) -> None:
        """Verify session missing record refuses and settles every peer."""
        with tempfile.TemporaryDirectory() as directory:
            session = self.session(Path(directory), "raise SystemExit(7)\n")
            refused = False
            try:
                try:
                    _ = session.line(5)
                except ToolError:
                    refused = True
            finally:
                session.close()
            self.assertTrue(refused)
            self.assertTrue(all(stream.closed for stream in session.streams))
            self.assertIsNotNone(session.child)
            if session.child is not None:
                self.assertEqual(session.child.returncode, 7)

    def test_session_extra_output_or_stderr_never_becomes_success(self) -> None:
        """Verify session extra output or stderr never becomes success."""
        for body in (
            "print('{}\\nEXTRA',flush=True)\n",
            "import sys\nprint('{}',flush=True)\nsys.stderr.write('PRIVATE')\n",
        ):
            with self.subTest(body=body), tempfile.TemporaryDirectory() as directory:
                session = self.session(Path(directory), body)
                refused = False
                try:
                    _ = session.line(5)
                    try:
                        _ = session.finish()
                    except ToolError:
                        refused = True
                finally:
                    session.close()
                self.assertTrue(refused)
                self.assertTrue(all(stream.closed for stream in session.streams))

    def test_session_frame_bound_refuses_before_writing_and_still_reaps(self) -> None:
        """Verify session frame bound refuses before writing and still reaps."""
        with tempfile.TemporaryDirectory() as directory:
            session = self.session(Path(directory), "import time\ntime.sleep(60)\n")
            refused = False
            try:
                try:
                    session.send({"url": "x" * 16384})
                except ToolError:
                    refused = True
            finally:
                session.close()
            self.assertTrue(refused)
            self.assertTrue(all(stream.closed for stream in session.streams))
            self.assertIsNotNone(session.child)
            if session.child is not None:
                self.assertIsNotNone(session.child.returncode)

    def authentication_failure(
        self, *, fail_server: bool, cleanup_error: bool = False
    ) -> tuple[dict[str, object], list[str]]:
        """Exercise whole-owner refusal before any guest can be admitted."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source, output = root / "source", root / "output"
            source.mkdir(mode=0o700)
            write(source / "target/debug/xtask", b"INERT_CONTROLLER", 0o755)
            write(
                source / "target/x86_64-unknown-linux-gnu/release/simplestchat-ops",
                b"INERT_CLIENT",
                0o755,
            )
            write(source / "Cargo.toml", b'[workspace.package]\nversion="0.1.0"\n')
            tools = root / "tools"
            for name in fetch_vm.TOOL_ROLES:
                write(tools / name, b"INERT_TOOL", 0o755)
            options = keeper.Options()
            options.run_id, options.run_attempt, options.artifact_id = 10, 1, 20
            options.output = output
            AuthenticationHost.events = []
            AuthenticationHost.fail_server = fail_server

            def selected(name: str) -> str:
                return str(tools / name)

            actual_finish = fetch_vm.Owners.finish

            def finish(
                owner: fetch_vm.Owners, control: fetch_vm.Control, report: dict[str, object]
            ) -> BaseException | None:
                result = actual_finish(owner, control, report)
                return OSError("PRIVATE_CLEANUP") if cleanup_error else result

            with (
                patch.object(keeper, "ROOT", source),
                patch.object(keeper, "Host", AuthenticationHost),
                patch.object(keeper, "SOURCE_INPUTS", ("Cargo.toml",)),
                patch.object(keeper, "admission", return_value=root),
                patch.object(vm, "preflight"),
                patch.object(keeper, "executable", side_effect=selected),
                patch.object(fetch_vm.Owners, "finish", finish),
                patch.dict(os.environ, CI, clear=True),
                patch.object(vm, "Guest") as guest,
            ):
                report = keeper.run(options, fetch_vm.Control())
            guest.assert_not_called()
            return report, list(AuthenticationHost.events)

    def test_saved_authentication_facts_do_not_replace_current_command_success(self) -> None:
        """Verify saved authentication facts do not replace current command success."""
        report, events = self.authentication_failure(fail_server=False)
        self.assertFalse(report["passed"])
        self.assertFalse(report["authenticatedOpsDistribution"])
        self.assertEqual(report["failure"], "ops-authentication")
        self.assertTrue(report["cleanupPassed"])
        self.assertEqual(events, ["authenticate-ops"])
        self.assertNotIn("PRIVATE", keeper.encoded(report).decode())

    def test_ops_authentication_does_not_substitute_for_server_four_subjects(self) -> None:
        """Verify ops authentication does not substitute for server four subjects."""
        report, events = self.authentication_failure(fail_server=True)
        self.assertFalse(report["passed"])
        self.assertTrue(report["authenticatedOpsDistribution"])
        self.assertEqual(report["failure"], "server-authentication")
        self.assertEqual(
            events, ["authenticate-ops", "authenticate-retained-ops", "authenticate-server"]
        )

    def test_cleanup_uncertainty_remains_negative_without_leaking_first_error(self) -> None:
        """Verify cleanup uncertainty remains negative without leaking first error."""
        report, events = self.authentication_failure(fail_server=False, cleanup_error=True)
        self.assertFalse(report["passed"])
        self.assertFalse(report["cleanupPassed"])
        self.assertEqual(report["failure"], "ops-authentication")
        self.assertEqual(events, ["authenticate-ops"])
        self.assertNotIn("PRIVATE", keeper.encoded(report).decode())

    def test_host_curl_profile_refuses_wrong_backend_or_old_version(self) -> None:
        """Admit only the actual fixed OpenSSL/HTTPS domain, without raw public output."""
        valid = b"curl 8.14.1 (x86_64) libcurl/8.14.1 OpenSSL/3.5.1\nProtocols: http https\n"
        self.assertEqual(guest.curl_version(valid), "8.14.1")
        for value in (
            valid.replace(b"8.14.1", b"8.3.0"),
            valid.replace(b"OpenSSL/3.5.1", b"GnuTLS/3.8.0"),
            valid.replace(b"http https", b"http"),
            b"PRIVATE_URL_VALUE",
        ):
            with self.subTest(value=value), self.assertRaises((ValueError, IndexError)):
                _ = guest.curl_version(value)

    def test_prepared_inventory_reuses_existing_original_and_refuses_changed_target(self) -> None:
        """Bind the existing install inventory, without exclusive recreation or target widening."""
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory).resolve()
            value = {
                "benchmark_hosts": {
                    "hosts": {
                        "vmfixture": {
                            "ansible_host": "127.0.0.1",
                            "ansible_port": 2222,
                            "ansible_user": "fixture",
                            "ansible_ssh_private_key_file": str(work / "client"),
                            "ansible_ssh_args": shlex.join(vm.ssh_options(work)),
                            "vm_fixture_run_id": "b" * 32,
                            "vm_fixture_revision": REVISION,
                        }
                    }
                }
            }
            write(work / "inventory.json", keeper.encoded(value))
            before = (work / "inventory.json").stat()
            owners = fetch_vm.Owners()
            control = fetch_vm.Control()
            refused = False
            try:
                selected = keeper.prepared_inventory(
                    owners, work, 2222, "b" * 32, REVISION, control
                )
                try:
                    _ = keeper.prepared_inventory(owners, work, 2223, "b" * 32, REVISION, control)
                except ToolError:
                    refused = True
            finally:
                control.settling = True
                failure = owners.finish(control, {})
            self.assertIsNone(failure)
            self.assertTrue(refused)
            self.assertEqual(selected, work / "inventory.json")
            self.assertEqual(before.st_ino, selected.stat().st_ino)
            self.assertTrue(all(original.stream.closed for original in owners.inputs))

    def test_workflow_is_manual_readonly_and_compile_precedes_credential(self) -> None:
        """Verify workflow is manual readonly and compile precedes credential."""
        value = obj(
            yaml_value(
                (ROOT / ".github/workflows/release-receive-runtime.yml").read_text(),
                scalars_as_strings=True,
            )
        )
        events = obj(value["on"])
        self.assertEqual(set(events), {"workflow_dispatch"})
        inputs = obj(obj(events["workflow_dispatch"])["inputs"])
        self.assertEqual(set(inputs), {"run_id", "run_attempt", "artifact_id"})
        for item in inputs.values():
            self.assertEqual(obj(item)["required"], "true")
            self.assertEqual(obj(item)["type"], "string")
        self.assertEqual(obj(value["permissions"]), {"contents": "read", "actions": "read"})
        jobs = obj(value["jobs"])
        job = obj(jobs["release-receive-runtime"])
        self.assertEqual(job["runs-on"], "ubuntu-24.04")
        steps = cast("list[dict[str, JsonValue]]", job["steps"])
        compile_index = next(
            index
            for index, item in enumerate(steps)
            if "Compile trusted" in str(item.get("name", ""))
        )
        credential_index = next(
            index
            for index, item in enumerate(steps)
            if "GH_TOKEN" in cast("dict[str, object]", item.get("env", {}))
        )
        self.assertLess(compile_index, credential_index)
        self.assertEqual(
            steps[0]["uses"], "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1"
        )
        upload = steps[-1]
        self.assertEqual(upload["if"], "${{ success() }}")
        self.assertEqual(
            string(obj(upload["with"])["path"]),
            "${{ runner.temp }}/release-receive-runtime/summary.json",
        )
        source = (ROOT / "build/security_vm_receive.py").read_text()
        self.assertNotIn("id-token", source)
        fixed = (ROOT / "ops/src/release/attestation/fetch/options.rs").read_text()
        self.assertIn(
            "/usr/bin/python3 -B /usr/local/libexec/simplestchat-public/fetch-release.py", fixed
        )


@final
class GrantResponse:
    """Inert fixed API headers; no network socket is created."""

    status = 302
    closed: bool = False

    def close(self) -> None:
        """Observe the distinct response owner even without reading its body."""
        self.closed = True

    def getheaders(self) -> list[tuple[str, str]]:
        """Return the sole policy-admitted target."""
        return [("Location", "https://fixture.blob.core.windows.net/a?sig=INERT")]


@final
class GrantConnection:
    """Observe only headers and checked close at the original API boundary."""

    def __init__(self) -> None:
        """Initialize closed capture fields, never an actual connection."""
        self.authorization = ""
        self.closed = False
        self.response: GrantResponse | http.client.HTTPResponse = GrantResponse()

    def request(self, method: str, path: str, *, headers: Mapping[str, str]) -> None:
        """Retain only the inert assertion's expected authorization spelling."""
        if method != "GET" or path != "/repos/swiftraccoon/simplestChat/actions/artifacts/123/zip":
            raise AssertionError
        self.authorization = headers["Authorization"]

    def getresponse(self) -> GrantResponse | http.client.HTTPResponse:
        """Return fixed metadata without a response body."""
        return self.response

    def close(self) -> None:
        """Record the actual finalizer invocation before any verdict."""
        self.closed = True


class ReceiveRepairTests(unittest.TestCase):
    """Exercise actual source-derived contracts behind review findings RV1-RV5."""

    def test_original_prepare_predicate_run_id_is_the_receiver_proof_selector(self) -> None:
        """Capture the unchanged producer's actual claim, then refuse the wrong key alias."""
        produced: list[JsonObject] = []
        original = attest.prepare

        def capture(artifact: Path, security: Path, selection: JsonObject) -> JsonObject:
            result = original(artifact, security, selection)
            produced.append(result)
            return result

        reference = prepare_reference.ReleaseAttestationTests()
        with patch.object(attest, "prepare", side_effect=capture):
            reference.test_producer_requires_success_and_binds_exact_original_export()
        self.assertEqual(len(produced), 1)
        claim = produced[0]
        keeper.server_attempt(claim, 789, 1)
        for invalid in (
            dict(claim, runId=None, buildRunId=789),
            dict(claim, runAttempt=2),
            dict(claim, runId=True),
        ):
            with self.subTest(invalid=invalid), self.assertRaises(ToolError):
                keeper.server_attempt(invalid, 789, 1)

    def test_main_late_restore_signal_invalidates_prior_positive_report(self) -> None:
        """Run the actual main fence with mocked global setters and a real late signal latch."""
        control = fetch_vm.Control()
        calls: list[int] = []

        def set_handler(number: signal.Signals, _handler: object) -> signal.Handlers:
            calls.append(int(number))
            if len(calls) == LATE_RESTORE_CALL:
                control.signal(signal.SIGINT, None)
            return signal.SIG_DFL

        def body(_options: keeper.Options, same: fetch_vm.Control) -> dict[str, object]:
            if same is not control:
                raise AssertionError
            same.settling = True
            return {"passed": True}

        with tempfile.TemporaryDirectory() as directory:
            output = io.BytesIO()
            console = io.TextIOWrapper(output)
            argv = [
                "fixture",
                "--run-id",
                "1",
                "--run-attempt",
                "1",
                "--artifact-id",
                "2",
                "--output",
                str(Path(directory) / "absent"),
            ]
            with (
                patch.object(sys, "argv", argv),
                patch.object(sys, "stdout", console),
                patch.object(fetch_vm, "Control", return_value=control),
                patch.object(keeper, "run", side_effect=body),
                patch.object(signal, "signal", side_effect=set_handler),
                patch.object(os, "umask", return_value=0o022),
            ):
                status = keeper.main()
            data = output.getvalue()
            console.close()
        self.assertEqual(status, 130)
        self.assertEqual(len(calls), 4)
        self.assertIn(b'"passed":true', data)

    def test_fresh_grant_uses_original_gh_despite_ambient_shadow(self) -> None:
        """Run the real bounded token adapter with owned A/B executables and an inert API."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source, shadow, output = root / "original/gh", root / "shadow/gh", root / "output"
            marker = root / "shadow-used"
            write(source, b"#!/bin/sh\nprintf INERT_PINNED_TOKEN\n", 0o755)
            write(
                shadow,
                ("#!/bin/sh\ntouch " + shlex.quote(str(marker)) + "\nprintf WRONG\n").encode(),
                0o755,
            )
            output.mkdir(mode=0o700)
            control = fetch_vm.Control()
            host = keeper.Host(output, control)
            before = set(output.iterdir())
            original = fetch_vm.Held.open(source, 4096, control)
            host.fixed_tools = {"gh": original}
            connection = GrantConnection()
            selected = reference.ReleaseSelection(
                repository=fetch_vm.REPOSITORY, revision=REVISION, artifact_id=123, ci_run=789
            )
            try:
                with (
                    patch.dict(
                        os.environ, {"PATH": str(shadow.parent), "GH_TOKEN": "INERT_CI_TOKEN"}
                    ),
                    patch.object(http.client, "HTTPSConnection", return_value=connection),
                ):
                    grant = keeper.fresh_grant(host, selected)
                original.check(control)
            finally:
                original.close()
            self.assertEqual(grant, "https://fixture.blob.core.windows.net/a?sig=INERT")
            self.assertEqual(connection.authorization, "Bearer INERT_PINNED_TOKEN")
            self.assertTrue(connection.closed)
            self.assertFalse(marker.exists())
            self.assertEqual(set(output.iterdir()), before)
            self.assertTrue(original.stream.closed)

    def test_all_authentication_roles_use_original_tools_before_any_vm_admission(self) -> None:
        """Run real inert child adapters for acquire, retained verification and server auth."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            output, tools_path, shadow = root / "output", root / "tools", root / "shadow"
            output.mkdir(mode=0o700)
            marker, bad = root / "original-used", root / "shadow-used"
            write(
                tools_path / "gh",
                (
                    "#!/bin/sh\numask 077\nprintf 'A\\n' >> " + shlex.quote(str(marker)) + "\n"
                ).encode(),
                0o755,
            )
            write(
                shadow / "gh",
                ("#!/bin/sh\numask 077\ntouch " + shlex.quote(str(bad)) + "\n").encode(),
                0o755,
            )
            for name in ("curl", "ssh"):
                write(tools_path / name, b"INERT_NOT_EXECUTED", 0o755)
            controller = root / "inert-controller"
            write(controller, b"#!/bin/sh\ngh fixed-inert-role >/dev/null\n", 0o755)
            control = fetch_vm.Control()
            owners = fetch_vm.Owners()
            host = keeper.Host(output, control)
            original_tools: dict[str, fetch_vm.Held] = {}
            for name in fetch_vm.TOOL_ROLES:
                held = fetch_vm.Held.open(tools_path / name, 4096, control)
                owners.inputs.append(held)
                original_tools[name] = held
            host.fixed_tools = original_tools
            owners.work = fetch_vm.Work.create(output)
            tools = fetch_vm.NativeTools.create(owners.work, original_tools, control)
            statuses: list[int] = []
            try:
                with patch.dict(os.environ, {"PATH": str(shadow), "GH_TOKEN": "INERT_CI_TOKEN"}):
                    environment = keeper.credential_environment(host, tools)
                    for role in (
                        "authenticate-ops",
                        "authenticate-retained-ops",
                        "authenticate-server",
                    ):
                        tools.check(control)
                        status, _ = host.command(role, [str(controller)], environment, 5, (0,))
                        statuses.append(status)
                        tools.check(control)
                data = marker.read_bytes()
            finally:
                control.settling = True
                cleanup = owners.finish(control, {})
            self.assertIsNone(cleanup)
            self.assertEqual(statuses, [0, 0, 0])
            self.assertEqual(data, b"A\n" * 3)
            self.assertFalse(bad.exists())
            self.assertTrue(all(original.stream.closed for original in original_tools.values()))

    def test_header_only_api_closes_detached_will_close_response_and_connection(self) -> None:
        """Settle an actual HTTPResponse makefile without consuming its ignored body."""

        class MemoryPipe(io.BytesIO):
            reads: int = 0

            @override
            def read(self, size: int | None = -1) -> bytes:
                self.reads += 1
                return super().read(size)

        @final
        class Socket:
            def __init__(self, pipe: MemoryPipe) -> None:
                self.pipe = pipe

            def makefile(self, _mode: str) -> MemoryPipe:
                return self.pipe

        pipe = MemoryPipe(
            b"HTTP/1.0 302 Found\r\nConnection: close\r\n"
            + b"Content-Length: 8\r\nLocation: https://fixture.blob.core.windows.net/a?sig=INERT\r\n"
            + b"\r\nIGNORED!"
        )
        response = http.client.HTTPResponse(cast("socket.socket", cast("object", Socket(pipe))))
        response.begin()
        will_close = response.will_close
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source, output = root / "gh", root / "output"
            write(source, b"INERT_NOT_EXECUTED", 0o755)
            output.mkdir(mode=0o700)
            control = fetch_vm.Control()
            host = keeper.Host(output, control)
            original = fetch_vm.Held.open(source, 4096, control)
            host.fixed_tools = {"gh": original}
            connection = GrantConnection()
            connection.response = response
            selected = reference.ReleaseSelection(
                repository=fetch_vm.REPOSITORY, revision=REVISION, artifact_id=123, ci_run=789
            )
            try:
                with (
                    patch.dict(os.environ, {"GH_TOKEN": "INERT_TOKEN"}),
                    patch.object(bounded_process, "run", return_value=(0, b"INERT_TOKEN", b"")),
                    patch.object(http.client, "HTTPSConnection", return_value=connection),
                ):
                    grant = keeper.fresh_grant(host, selected)
                closed_by_operation = response.isclosed()
            finally:
                response.close()
                original.close()
            self.assertTrue(will_close)
            self.assertTrue(closed_by_operation)
            self.assertTrue(connection.closed)
            self.assertEqual(pipe.reads, 0)
            self.assertEqual(grant, "https://fixture.blob.core.windows.net/a?sig=INERT")

    def test_response_close_fault_attempts_connection_close_and_preserves_first_cause(self) -> None:
        """Keep both HTTP peers owned when the response close reports uncertainty."""
        for invalid_status in (False, True):
            with (
                self.subTest(invalid_status=invalid_status),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = Path(directory).resolve()
                source, output = root / "gh", root / "output"
                write(source, b"INERT_NOT_EXECUTED", 0o755)
                output.mkdir(mode=0o700)
                control = fetch_vm.Control()
                host = keeper.Host(output, control)
                original = fetch_vm.Held.open(source, 4096, control)
                host.fixed_tools = {"gh": original}
                connection = GrantConnection()
                response = GrantResponse()
                if invalid_status:
                    response.status = 500
                connection.response = response
                real_close = response.close

                def uncertain_close(close: Callable[[], None] = real_close) -> None:
                    close()
                    message = "INERT_CLOSE_UNCERTAINTY"
                    raise OSError(message)

                selected = reference.ReleaseSelection(
                    repository=fetch_vm.REPOSITORY, revision=REVISION, artifact_id=123, ci_run=789
                )
                failure: BaseException | None = None
                try:
                    with (
                        patch.dict(os.environ, {"GH_TOKEN": "INERT_TOKEN"}),
                        patch.object(bounded_process, "run", return_value=(0, b"INERT_TOKEN", b"")),
                        patch.object(http.client, "HTTPSConnection", return_value=connection),
                        patch.object(response, "close", side_effect=uncertain_close),
                    ):
                        try:
                            _ = keeper.fresh_grant(host, selected)
                        except (OSError, ToolError) as error:
                            failure = error
                finally:
                    real_close()
                    original.close()
                self.assertIsInstance(failure, ToolError if invalid_status else OSError)
                self.assertTrue(response.closed)
                self.assertTrue(connection.closed)
                self.assertTrue(original.stream.closed)

    def test_guest_projection_rejects_extra_secret_fields_and_wrong_negative_status(self) -> None:
        """Keep both success and expected-negative public projections closed at the controller."""
        lock = {
            "case": "lock-negative",
            "nativeExitStatus": 1,
            "noReady": True,
            "noNewAttempt": True,
            "publicationUnchanged": True,
            "lockReleased": True,
        }
        self.assertEqual(keeper.guest_observation("lock-negative", lock), lock)
        for invalid in (
            dict(lock, extra="INERT_OPAQUE_GRANT"),
            dict(lock, nativeExitStatus=0),
            dict(lock, nativeExitStatus=True),
            dict(lock, noReady=False),
        ):
            with self.subTest(invalid=invalid), self.assertRaises(ToolError):
                _ = keeper.guest_observation("lock-negative", invalid)
        for phase in ("arm-conflict", "restore-conflict"):
            value = {"conflictRestored": phase == "restore-conflict", "fixtureOnly": True}
            self.assertEqual(keeper.guest_observation(phase, value), value)
            with self.assertRaises(ToolError):
                _ = keeper.guest_observation(phase, dict(value, extra="INERT_PRIVATE_PATH"))
        pins = {"binarySha256": "a" * 64, "routeSha256": "b" * 64}
        installed = {
            "installedRoute": True,
            **pins,
            "timeoutSeconds": 300,
            "finalizationSeconds": 30,
            "hostCurl": {
                "sha256": "c" * 64,
                "bytes": 100,
                "version": "8.14.1",
                "opensslAdvertised": True,
                "caSha256": "d" * 64,
                "caBytes": 100,
            },
        }
        _ = keeper.guest_observation("install-route", installed, pins)
        with self.assertRaises(ToolError):
            _ = keeper.guest_observation(
                "install-route", dict(installed, hostCurl={"extra": "INERT_OPAQUE_TARGET"}), pins
            )
        expected: dict[str, object] = {
            "runId": "b" * 32,
            "revision": REVISION,
            "published": {
                "image.tar": {"sha256": "c" * 64, "bytes": 4, "device": 1, "inode": 2},
                "release.json": {"sha256": "d" * 64, "bytes": 4, "device": 1, "inode": 3},
            },
        }
        final = {
            "schemaVersion": 1,
            "scope": "SignedMuslReceiveGuestProbe",
            **expected,
            "classes": {"complete": 2, "retained_artifact_differs": 1, "zip_checksum_mismatch": 1},
            "lockReleased": True,
            "passed": True,
        }
        projected = keeper.guest_observation("inspect", final, expected)
        self.assertNotIn("INERT", keeper.encoded(projected).decode())
        with self.assertRaises(ToolError):
            _ = keeper.guest_observation(
                "inspect", dict(final, extra="INERT_OPAQUE_GRANT"), expected
            )
        with self.assertRaises(ToolError):
            _ = keeper.guest_observation(
                "inspect",
                dict(
                    final,
                    classes={
                        "complete": 2,
                        "retained_artifact_differs": True,
                        "zip_checksum_mismatch": 1,
                    },
                ),
                expected,
            )

    def test_late_ops_tree_admission_never_opens_a_descriptor(self) -> None:
        """Fence the original body before acquiring another namespace owner."""
        late = fetch_vm.Control(started=time.monotonic() - 2000)
        with patch.object(os, "open") as opened, self.assertRaises(ToolError):
            _ = keeper.OpsTree(Path("/inert-not-opened"), late)
        opened.assert_not_called()

    def test_ops_tree_retains_namespace_and_refuses_new_member(self) -> None:
        """Keep the original directory through a real persistent namespace mutation."""
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory).resolve()
            path.chmod(0o700)
            for index in range(15):
                write(path / f"file-{index}", b"inert")
            control = fetch_vm.Control()
            tree = keeper.OpsTree(path, control)
            write(path / "foreign", b"changed")
            refused = False
            try:
                tree.check()
            except ToolError:
                refused = True
            close_refused = False
            try:
                tree.finish()
            except ToolError:
                close_refused = True
            self.assertTrue(refused)
            self.assertTrue(close_refused)
            self.assertEqual(tree.descriptor, -1)


@final
class WholeHost(keeper.Host):
    """Adapt only actual command/VM boundaries while keeping file and transfer owners real."""

    def __init__(self, output: Path, control: fetch_vm.Control) -> None:
        """Retain original keeper environment and source-selected inert payload bytes."""
        super().__init__(output, control)
        self.qemu_status: int = 0
        self.close_fault = False
        self.conflict_error: BaseException | None = None
        self.zip = fetch_data.make_zip()
        self.source_files = fetch_data.fixture_files()
        self.native_calls: list[str] = []

    @override
    def command(
        self,
        name: str,
        argv: Sequence[str],
        environment: Mapping[str, str],
        timeout: float,
        accepted: tuple[int, ...],
    ) -> tuple[int, bytes]:
        """Keep genuine input trees and full controller receipts around inert child statuses."""
        self.control.check()
        _ = environment, timeout, accepted
        if name == "authenticate-ops":
            names = [
                "simplestchat-ops",
                "ops-release.json",
                "ops-evidence.json",
                "ops-predicate.json",
                "ops-preparation.json",
                "ops-attestation.jsonl",
            ]
            names.extend(f"ops-evidence-{index:02d}.bin" for index in range(9))
            for member in names:
                write(self.output / "ops-acquired/input" / member, b"INERT_SIGNED_PAYLOAD")
            return 0, b""
        if name == "authenticate-retained-ops":
            request = keeper.private_record(Path(argv[4]))
            if request.get("authentication") != "gh-token":
                raise AssertionError
            return 0, b""
        if name == "authenticate-server":
            acquired = self.output / "server-acquired"
            for member, data in self.source_files.items():
                write(acquired / "artifact" / member, data)
            write(acquired / "artifact/artifact.zip", self.zip)
            write(
                acquired / "selection.json",
                keeper.encoded({"revision": REVISION, "ciRunId": 789, "artifactId": 123}),
            )
            return 0, b""
        if name in ("cached-local-first", "local-download-repeat", "publication-conflict"):
            self.native_calls.append(name)
            if name == "publication-conflict" and self.conflict_error is not None:
                raise self.conflict_error
            selected = Path(argv[argv.index("--output-parent") + 1]) / "release-fetch.fixture"
            write(selected / "artifact.json", keeper.encoded(fetch_data.envelope(self.zip)))
            remote: dict[str, object] = {
                "schemaVersion": 1,
                "passed": True,
                "settled": True,
                "finalized": True,
                "status": "complete",
                "phase": "complete",
                "failureClass": None,
                "revision": REVISION,
                "artifactId": 123,
                "zipSha256": hashlib.sha256(self.zip).hexdigest(),
                "artifactZipBytes": len(self.zip),
                "downloadedBytes": len(self.zip),
                "archiveSha256": hashlib.sha256(self.source_files["image.tar"]).hexdigest(),
                "manifestSha256": hashlib.sha256(self.source_files["release.json"]).hexdigest(),
                "elapsedSeconds": 1,
                "evidence": "INERT_PRIVATE_ATTEMPT",
            }
            passed = name != "publication-conflict"
            if not passed:
                remote.update(
                    passed=False,
                    finalized=False,
                    status="failed",
                    phase="publish",
                    failureClass="retained_artifact_differs",
                )
            return (0 if passed else 1), keeper.encoded(
                {
                    "schemaVersion": 1,
                    "operation": "fetch_release_controller",
                    "passed": passed,
                    "phase": "complete" if passed else "remote_fetch",
                    "remoteSettled": True,
                    "failureClass": None if passed else "remote_fetch_failed",
                    "remote": remote,
                }
            )
        raise AssertionError

    @override
    def run(
        self,
        name: str,
        argv: Sequence[str],
        *,
        timeout: float = 120,
        accepted: tuple[int, ...] = (0,),
        input_file: BinaryIO | None = None,
    ) -> tuple[int, bytes]:
        """Model only the nonexecuting measurement and trusted fixture-stage boundary."""
        _ = timeout, accepted, input_file
        if name == "measure-signed-executable":
            data = Path(argv[-1]).read_bytes()
            fields = {
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
            value: dict[str, object] = dict.fromkeys(fields, True)
            value.update(
                sha256=hashlib.sha256(data).hexdigest(),
                bytes=len(data),
                architecture="x86_64",
                elfClass=64,
                fileType="dynamic",
                programHeaders=1,
                neededCount=0,
            )
            return 0, keeper.encoded(value)
        if name == "receive-stage":
            return (
                0,
                b"vmfixture : ok=3 changed=2 unreachable=0 failed=0 "
                + b"skipped=0 rescued=0 ignored=0\n",
            )
        raise AssertionError


class ReceiveWholeTests(unittest.TestCase):
    """Run the entire keeper over real owned files and explicitly inert external observations."""

    def exercise(  # noqa: C901, PLR0915 -- One closed fixture keeps real original owners together.
        self,
        *,
        qemu_status: int = 0,
        changed_repeat: bool = False,
        conflict: tuple[BaseException | None, BaseException] | None = None,
        settlements: list[tuple[BaseException | None, BaseException | None]] | None = None,
    ) -> tuple[dict[str, object], list[str], bool]:
        """Settle all real source/tree/transfer/known-host owners before returning facts."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source, output, home = root / "source", root / "output", root / "ssh-home"
            source.mkdir(mode=0o700)
            write(source / "build/security_vm_receive_route.py", b"INERT_WRAPPER_NOT_EXECUTED")
            write(source / "ops/ansible/files/fetch-release.py", b"INERT_ORIGINAL_ENTRY")
            home.mkdir(mode=0o700)
            write(source / "Cargo.toml", b'[workspace.package]\nversion="0.1.0"\n')
            write(source / "target/debug/xtask", b"INERT_CONTROLLER", 0o755)
            write(
                source / "target/x86_64-unknown-linux-gnu/release/simplestchat-ops",
                b"INERT_CLIENT",
                0o755,
            )
            tools = root / "tools"
            for name in (*fetch_vm.TOOL_ROLES, "ansible-playbook"):
                write(tools / name, b"INERT_NOT_EXECUTED", 0o755)
            options = keeper.Options()
            options.run_id, options.run_attempt, options.artifact_id = 789, 1, 456
            options.output = output
            hosts: list[WholeHost] = []
            closed: list[bool] = []
            phases: list[str] = []
            run_id = ""
            files = fetch_data.fixture_files()

            def published(count: int = 1) -> dict[str, object]:
                return {
                    name: {
                        "sha256": hashlib.sha256(files[name]).hexdigest(),
                        "bytes": len(files[name]),
                        "device": 1,
                        "inode": index + int(changed_repeat and count == len(fetch_vm.TRIALS)),
                    }
                    for index, name in enumerate(("image.tar", "release.json"), 1)
                }

            def make_host(path: Path, control: fetch_vm.Control) -> WholeHost:
                result = WholeHost(path, control)
                result.qemu_status = qemu_status
                result.conflict_error = conflict[0] if conflict is not None else None
                hosts.append(result)
                return result

            def seed(_host: keeper.Host, work: Path, _port: int, _run: str) -> None:
                write(work / "client", b"INERT_KEY")
                write(work / "host.pub", ("ssh-ed25519 " + "A" * 68 + " vm-fixture\n").encode())

            def stage(
                _host: keeper.Host, _guest: object, work: Path, port: int, values: ops_guest.Options
            ) -> None:
                _ = ops_vm.inventory(work, port, values)

            def installed(
                _host: keeper.Host, _guest: object, _work: Path, _port: int, _archive: ops_vm.Input
            ) -> dict[str, object]:
                return {
                    "passed": True,
                    "cases": [
                        {"name": name, "exitStatus": status, "passed": True}
                        for name, status in ops_guest.CASES
                    ],
                    "install": {"firstChanged": True, "secondChanged": False},
                }

            def call(
                _host: keeper.Host,
                _work: Path,
                _port: int,
                phase: str,
                value: object = None,
                *,
                expected: Mapping[str, object] | None = None,
            ) -> dict[str, object]:
                nonlocal run_id
                _ = expected
                phases.append(phase)
                if phase == "restore-conflict" and conflict is not None:
                    raise conflict[1]
                if phase == "install-route":
                    pin = cast("dict[str, object]", value)
                    run_id = str(pin["runId"])
                    return {
                        "installedRoute": True,
                        "binarySha256": pin["binarySha256"],
                        "routeSha256": pin["routeSha256"],
                        "timeoutSeconds": 300,
                        "finalizationSeconds": 30,
                        "hostCurl": {
                            "sha256": "c" * 64,
                            "bytes": 100,
                            "version": "8.14.1",
                            "opensslAdvertised": True,
                            "caSha256": "d" * 64,
                            "caBytes": 100,
                        },
                    }
                if phase == "inspect":
                    return {
                        "schemaVersion": 1,
                        "scope": "SignedMuslReceiveGuestProbe",
                        "runId": run_id,
                        "revision": REVISION,
                        "published": published(),
                        "classes": {
                            "complete": 2,
                            "retained_artifact_differs": 1,
                            "zip_checksum_mismatch": 1,
                        },
                        "lockReleased": True,
                        "passed": True,
                    }
                if phase in ("arm-conflict", "restore-conflict"):
                    return {"conflictRestored": phase == "restore-conflict", "fixtureOnly": True}
                return {
                    "case": phase,
                    "nativeExitStatus": 1,
                    "noReady": True,
                    "noNewAttempt": True,
                    "publicationUnchanged": True,
                    "lockReleased": True,
                }

            def observation(
                _host: keeper.Host,
                _work: Path,
                _port: int,
                _run: str,
                _selected: Mapping[str, object],
                _pins: Mapping[str, fetch_vm.Held],
                count: int,
            ) -> dict[str, object]:
                return {"published": published(count), "passed": True}

            def tool(name: str) -> str:
                return str(tools / name)

            original_settle = fetch_vm.settle

            def settle(
                actions: Sequence[Callable[[], None]], first: BaseException | None = None
            ) -> BaseException | None:
                result = original_settle(actions, first)
                if settlements is not None and any(
                    action.__name__ == "restore_conflict" for action in actions
                ):
                    settlements.append((first, result))
                return result

            original_close = fetch_vm.Held.close

            def close(value: fetch_vm.Held) -> None:
                original_close(value)
                closed.append(value.stream.closed)

            with (
                patch.object(keeper, "ROOT", source),
                patch.object(
                    keeper,
                    "SOURCE_INPUTS",
                    (
                        "Cargo.toml",
                        "build/security_vm_receive_route.py",
                        "ops/ansible/files/fetch-release.py",
                    ),
                ),
                patch.object(keeper, "Host", side_effect=make_host),
                patch.object(keeper, "admission", return_value=home),
                patch.object(keeper, "executable", side_effect=tool),
                patch.object(vm, "preflight"),
                patch.object(vm, "prepare_disk"),
                patch.object(vm, "create_seed", side_effect=seed),
                patch.object(vm, "boot"),
                patch.object(vm, "qemu_command", return_value=["INERT_QEMU"]),
                patch.object(vm, "Guest", fetch_reference.FakeGuest),
                patch.object(
                    vm.CloudImage,
                    "read",
                    return_value=vm.CloudImage("https://example.invalid/inert", "a" * 128),
                ),
                patch.object(ops_vm, "stage", side_effect=stage),
                patch.object(ops_vm, "exercise", side_effect=installed),
                patch.object(keeper, "call_guest", side_effect=call),
                patch.object(fetch_vm, "observe_guest", side_effect=observation),
                patch.object(
                    keeper,
                    "checksum_negative",
                    return_value={"case": "retained-download-checksum", "passed": True},
                ),
                patch.object(fetch_vm.Held, "close", close),
                patch.object(fetch_vm, "settle", side_effect=settle),
                patch.dict(os.environ, CI, clear=True),
            ):
                report = keeper.run(options, fetch_vm.Control())
            return (
                report,
                phases,
                bool(closed) and all(closed) and not (home / ".ssh/known_hosts").exists(),
            )

    def test_complete_owned_flow_reaches_both_clients_negatives_and_final_probe(self) -> None:
        """Exercise actual source/USTAR/selection custody across every complete body phase."""
        report, phases, closed = self.exercise()
        self.assertTrue(closed)
        self.assertTrue(report["passed"])
        self.assertTrue(report["cleanupPassed"])
        self.assertEqual(report["opsVerifierCalls"], 6)
        self.assertEqual(
            phases,
            [
                "install-route",
                "lock-negative",
                "journal-negative",
                "arm-conflict",
                "restore-conflict",
                "inspect",
            ],
        )
        self.assertEqual(report["install"], {"firstChanged": True, "secondChanged": False})

    def test_nonzero_qemu_status_invalidates_complete_body_and_still_closes_inputs(self) -> None:
        """Refuse a reaped but nonzero emulator after all successful body observations."""
        report, phases, closed = self.exercise(qemu_status=1)
        self.assertTrue(closed)
        self.assertIn("inspect", phases)
        self.assertFalse(report["passed"])
        self.assertFalse(report["cleanupPassed"])

    def test_conflict_restore_keeps_known_body_identity_and_never_creates_success(self) -> None:
        """Always attempt restoration; preserve body identity when both body and restore fail."""
        primary = ToolError("INERT_CONFLICT_BODY")
        secondary = OSError("INERT_RESTORE_CLOSE")
        for body in (primary, None):
            with self.subTest(body=body):
                observations: list[tuple[BaseException | None, BaseException | None]] = []
                report, phases, closed = self.exercise(
                    conflict=(body, secondary), settlements=observations
                )
                self.assertTrue(closed)
                self.assertFalse(report["passed"])
                self.assertIn("restore-conflict", phases)
                self.assertNotIn("inspect", phases)
                self.assertEqual(len(observations), 1)
                self.assertIs(observations[0][0], body)
                self.assertIs(observations[0][1], body if body is not None else secondary)

    def test_repeat_inode_change_is_negative_despite_native_zero(self) -> None:
        """Keep independent original inode facts authoritative over a green child receipt."""
        report, phases, closed = self.exercise(changed_repeat=True)
        self.assertTrue(closed)
        self.assertFalse(report["passed"])
        self.assertNotIn("inspect", phases)


class ReceiveFirstCauseTests(unittest.TestCase):
    """Pair actual original closes with an already observed body failure."""

    def test_invalid_ready_keeps_body_failure_after_actual_session_close_fault(self) -> None:
        """Reap the real inert SSH-shaped child and close all pipes before checking primary."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            output = root / "output"
            output.mkdir(mode=0o700)
            peer, ssh = root / "peer.py", root / "ssh"
            write(
                peer,
                b"import sys,time\n"
                + b"sys.stdin.buffer.readline()\n"
                + b'print(\'{"schemaVersion":1,"status":"wrong"}\',flush=True)\n'
                + b"time.sleep(60)\n",
            )
            write(
                ssh,
                (
                    "#!/bin/sh\nexec "
                    + shlex.quote(sys.executable)
                    + " -I -B "
                    + shlex.quote(str(peer))
                    + "\n"
                ).encode(),
                0o755,
            )
            control = fetch_vm.Control()
            host = keeper.Host(output, control)
            original = fetch_vm.Held.open(ssh, 4096, control)
            host.fixed_tools = {"ssh": original}
            sessions: list[keeper.Session] = []
            real_close = keeper.Session.close
            secondary = OSError("INERT_SECONDARY_CLOSE")

            def uncertain_close(value: keeper.Session) -> None:
                sessions.append(value)
                real_close(value)
                raise secondary

            selected = {"revision": REVISION, "artifactId": 123, "ciRunId": 789}
            failure: BaseException | None = None
            try:
                with (
                    patch.object(keeper.Session, "close", new=uncertain_close),
                    patch.object(keeper, "fresh_grant") as grant,
                    patch.dict(
                        os.environ, {"PATH": "/usr/bin:/bin", "HOME": str(root)}, clear=True
                    ),
                ):
                    try:
                        _ = keeper.checksum_negative(
                            host,
                            root,
                            2222,
                            selected,
                            cast("dict[str, object]", fetch_data.envelope()),
                        )
                    except (OSError, ToolError) as error:
                        failure = error
                grant.assert_not_called()
                facts = [
                    (
                        all(stream.closed for stream in session.streams),
                        session.child is not None and session.child.returncode is not None,
                    )
                    for session in sessions
                ]
            finally:
                for session in sessions:
                    if session.child is not None and session.child.returncode is None:
                        bounded_process.stop(session.child, grace=1)
                    for stream in session.streams:
                        stream.close()
                original.close()
            self.assertIsInstance(failure, ToolError)
            self.assertIsNot(failure, secondary)
            self.assertEqual(facts, [(True, True)])

    def test_constructor_register_failure_closes_all_existing_pipes_and_keeps_primary(self) -> None:
        """Admit all three original pipes before the first failing selector registration."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            peer = root / "peer.py"
            write(peer, b"import time\ntime.sleep(60)\n")
            real_selector = selectors.DefaultSelector
            initial = real_selector()
            selectors_created: list[selectors.BaseSelector] = []

            def selector() -> selectors.BaseSelector:
                result = initial if not selectors_created else real_selector()
                selectors_created.append(result)
                return result

            primary = ValueError("INERT_REGISTER_PRIMARY")
            secondary = OSError("INERT_CLOSE_SECONDARY")
            sessions: list[keeper.Session] = []
            real_close = keeper.Session.close

            def uncertain_close(value: keeper.Session) -> None:
                sessions.append(value)
                real_close(value)
                raise secondary

            failure: BaseException | None = None
            try:
                with (
                    patch.object(selectors, "DefaultSelector", side_effect=selector),
                    patch.object(initial, "register", side_effect=primary),
                    patch.object(keeper.Session, "close", new=uncertain_close),
                ):
                    try:
                        _ = keeper.Session(
                            [sys.executable, "-I", "-B", str(peer)],
                            {"PATH": "/usr/bin:/bin"},
                            fetch_vm.Control(),
                        )
                    except (ValueError, OSError) as error:
                        failure = error
                facts = [
                    (
                        len(session.streams),
                        all(stream.closed for stream in session.streams),
                        session.child is not None and session.child.returncode is not None,
                        session.child is not None
                        and session.child.stderr is not None
                        and session.child.stderr.closed,
                    )
                    for session in sessions
                ]
            finally:
                for session in sessions:
                    if session.child is not None and session.child.returncode is None:
                        bounded_process.stop(session.child, grace=1)
                    for stream in session.streams:
                        stream.close()
                for item in selectors_created:
                    item.close()
            self.assertIs(failure, primary)
            self.assertEqual(facts, [(3, True, True, True)])

    def test_transfer_admission_failure_closes_both_loan_and_duplicate_without_primary_loss(
        self,
    ) -> None:
        """Use the real regular transfer and duplicated Input before injecting admission failure."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            output = root / "output"
            output.mkdir(mode=0o700)
            control = fetch_vm.Control()
            host = WholeHost(output, control)
            owners = fetch_vm.Owners()
            verified: dict[str, fetch_vm.Held] = {}
            for name in ("ops-release.json", "simplestchat-ops"):
                write(root / name, b"INERT_CURRENT_PAYLOAD")
                held = fetch_vm.Held.open(root / name, 4096, control)
                owners.inputs.append(held)
                verified[name] = held
            selection: dict[str, object] = {
                "repository": fetch_vm.REPOSITORY,
                "revision": REVISION,
                "packageVersion": "0.1.0",
                "target": "x86_64-unknown-linux-musl",
                "protocolVersion": 1,
                "buildRunId": 789,
                "runAttempt": 1,
            }
            original_type = ops_vm.Input
            real_close = original_type.close
            primary, secondary = ValueError("INERT_ADMIT_PRIMARY"), OSError("INERT_CLOSE_SECONDARY")
            inputs: list[ops_vm.Input] = []
            borrowed: list[int] = []

            def constructor(descriptor: int, options: ops_guest.Options) -> ops_vm.Input:
                borrowed.append(descriptor)
                result = original_type(descriptor, options)
                inputs.append(result)
                return result

            def uncertain_close(value: ops_vm.Input) -> None:
                real_close(value)
                raise secondary

            failure: BaseException | None = None
            closed = False
            try:
                with (
                    patch.object(original_type, "admit", side_effect=primary),
                    patch.object(original_type, "close", new=uncertain_close),
                    patch.object(ops_vm, "Input", side_effect=constructor),
                ):
                    try:
                        _ = keeper.transfer(
                            host,
                            owners,
                            verified,
                            selection,
                            root / "unused-client",
                            output,
                            "b" * 32,
                            456,
                        )
                    except (ValueError, OSError) as error:
                        failure = error
                try:
                    _ = os.fstat(borrowed[0])
                except OSError:
                    closed = True
                duplicates_closed = all(value.file.closed for value in inputs)
            finally:
                for value in inputs:
                    real_close(value)
                control.settling = True
                cleanup = owners.finish(control, {})
            self.assertIsNone(cleanup)
            self.assertIs(failure, primary)
            self.assertTrue(closed)
            self.assertTrue(duplicates_closed)


if __name__ == "__main__":
    _ = unittest.main()
