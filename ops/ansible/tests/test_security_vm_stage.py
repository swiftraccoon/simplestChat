"""Inert Stage-keeper custody, strict evidence and cleanup tests; no engine/guest/network."""

from __future__ import annotations

import copy
import hashlib
import io
import json
import os
import signal
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import TYPE_CHECKING, cast, final
from unittest.mock import patch

from test_support import ROOT

# isort: split
import security_vm_fetch as fetch_vm
import security_vm_stage as keeper
import security_vm_stage_guest as guest
import test_release_build as image_fixture
from security_tools import ToolError

if TYPE_CHECKING:
    from collections.abc import Mapping, Sequence
    from types import TracebackType
    from typing import BinaryIO

    import security_vm_receive as receive

REVISION = "a" * 40
TOKEN = "b" * 32
IMAGE = "sha256:" + "c" * 64
ACTOR = "d" * 64
FIRST_RESTORE_CALL = 3


def encoded(value: object) -> bytes:
    """Pretty output deliberately tests whole-object parsing, not one-LF framing."""
    return (json.dumps(value, indent=2) + "\n").encode()


def actor_events(
    *,
    identifier: str = ACTOR,
    name: str | None = None,
    image: str = IMAGE,
    actions: tuple[str, ...] = ("create", "start", "die", "destroy"),
) -> bytes:
    """Construct actual Docker event-shaped rows without invoking its engine."""
    rows: list[str] = []
    for action in actions:
        attributes = {"name": name or "scpub-release-validate-" + TOKEN, "image": image}
        if action == "die":
            attributes["exitCode"] = "0"
        rows.append(
            json.dumps(
                {
                    "Type": "container",
                    "Action": action,
                    "Actor": {"ID": identifier, "Attributes": attributes},
                }
            )
        )
    return ("\n".join(rows) + "\n").encode()


def guest_result() -> tuple[dict[str, object], dict[str, object]]:
    """Return the fixed qualified projection and independently supplied manifest pin."""
    manifest = hashlib.sha256(b"ORIGINAL").hexdigest()
    value: dict[str, object] = {
        "schemaVersion": 1,
        "scope": "SignedMuslStageGuest",
        "runId": TOKEN,
        "revision": REVISION,
        "passed": True,
        "firstExitStatus": 0,
        "repeatExitStatus": 0,
        "staged": {"sha256": "e" * 64, "bytes": 20, "device": 1, "inode": 4},
        "manifestSha256": manifest,
        "imageId": IMAGE,
        "nativeActorId": ACTOR,
        "nativeActorLifecycleObserved": True,
        "nativeValidatorIsolationIndependentlyInspected": False,
        "firstEventCount": 4,
        "cumulativeEventCount": 4,
        "repeatStagedOriginalUnchanged": True,
        "repeatSkippedLoadAndValidator": True,
        "migrationMapMatched": True,
        "knownContainersAbsent": True,
        "protectedFilesUnchanged": 4,
        "workloadLockReacquired": True,
        "commands": 25,
        "separateProbe": {
            "passed": True,
            "isolationIndependentlyInspected": True,
            "migrationMapMatched": True,
            "exitStatus": 0,
            "containerId": "f" * 64,
        },
    }
    return value, {"runId": TOKEN, "revision": REVISION, "manifest": {"sha256": manifest}}


class StageProofTests(unittest.TestCase):
    """Finite concrete evidence/ownership faults leave authentication and success closed."""

    def test_projection_pretty_json_and_extra_secret_rejection(self) -> None:
        """Whole pretty JSON works, while unknown guest output never reaches summary."""
        value, request = guest_result()
        self.assertEqual(keeper.guest_projection(encoded(value), request), value)
        with self.assertRaises(ValueError):
            _ = keeper.guest_projection(
                encoded(dict(value, opaque="PRIVATE_SIGNED_TARGET")), request
            )
        changed = copy.deepcopy(value)
        cast("dict[str, object]", changed["separateProbe"])["extra"] = "PRIVATE_SECRET"
        with self.assertRaises(ValueError):
            _ = keeper.guest_projection(encoded(changed), request)

    def test_native_isolation_cannot_be_inferred_from_separate_probe(self) -> None:
        """The exact source qualification is a required public fact."""
        value, request = guest_result()
        for change in (
            {"nativeValidatorIsolationIndependentlyInspected": True},
            {"manifestSha256": "0" * 64},
            {"firstExitStatus": False},
            {"cumulativeEventCount": 256},
        ):
            with self.subTest(change=change), self.assertRaises((ValueError, ToolError)):
                _ = keeper.guest_projection(encoded(dict(value, **change)), request)

    def test_actual_actor_anchor_requires_complete_zero_lifecycle(self) -> None:
        """A missed first anchor, duplicate repeat or mismatched immutable image refuses."""
        name = "scpub-release-validate-" + TOKEN
        result = guest.actor_sequence(actor_events(), name, IMAGE)
        self.assertEqual(result, (ACTOR, ["create", "start", "die", "destroy"], 4))
        for data in (
            actor_events(actions=("start", "die", "destroy")),
            actor_events() + actor_events(identifier="e" * 64),
            actor_events(image="sha256:" + "0" * 64),
            actor_events().replace(b'"exitCode": "0"', b'"exitCode": "1"'),
            actor_events() + actor_events(name="scpub-release-validate-" + "f" * 32),
            b"{}\n" * 256,
        ):
            with self.subTest(data=data[:50]), self.assertRaises(ValueError):
                _ = guest.actor_sequence(data, name, IMAGE, ACTOR)

    def test_migration_rows_are_unique_and_fixed_path(self) -> None:
        """Real SHA384/version normalization is compared without accepting arbitrary output."""
        sha = hashlib.sha384(b"SELECT 1;").hexdigest()
        valid = (sha + "  /app/migrations/0001_first.sql\n").encode()
        self.assertEqual(guest.migration_map(valid), {"1": sha})
        for data in (valid + valid, b"", valid.replace(b"/app/", b"/tmp/"), b"opaque secret\n"):
            with self.subTest(data=data[:20]), self.assertRaises(ValueError):
                _ = guest.migration_map(data)

    def test_native_budget_does_not_refresh_original_window(self) -> None:
        """Both trials clip from their same original body and reserve M60 plus work."""
        control = guest.Control(1000)
        start = control.started
        with patch("security_vm_stage_guest.time.monotonic", return_value=start + 10):
            self.assertEqual(guest.native_budget(control), 600)
        with patch("security_vm_stage_guest.time.monotonic", return_value=start + 850):
            self.assertEqual(guest.native_budget(control), 130)
        with (
            patch("security_vm_stage_guest.time.monotonic", return_value=start + 895),
            self.assertRaises(ValueError),
        ):
            _ = guest.native_budget(control)
        self.assertEqual(control.body_end, start + 1000)

    def test_server_namespace_retains_original_and_all_eleven_members(self) -> None:
        """Real own-temp directory exchanges are rejected before a verifier can run."""
        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary).resolve()
            path = parent / "artifact"
            path.mkdir(mode=0o700)
            for name in keeper.SERVER_NAMES:
                _ = (path / name).write_bytes(b"INERT")
                (path / name).chmod(0o600)
            control = fetch_vm.Control()
            tree = keeper.ServerTree(path, control)
            failure: BaseException | None = None
            try:
                tree.check()
                _ = path.rename(parent / "old")
                path.mkdir(mode=0o700)
                for name in keeper.SERVER_NAMES:
                    _ = (path / name).write_bytes(b"INERT")
                try:
                    tree.check()
                except ToolError as error:
                    failure = error
            finally:
                with self.assertRaises(ToolError):
                    tree.finish()
            self.assertIsNotNone(failure)
            self.assertEqual(tree.descriptor, -1)

    def test_original_constructor_failure_explicitly_closes_admitted_file(self) -> None:
        """An admitted real file cannot leak when root ownership is refused."""
        if os.getuid() == 0:
            self.skipTest("The negative specifically requires an unprivileged own-temp owner.")
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "original"
            _ = path.write_bytes(b"INERT")
            path.chmod(0o600)
            opened: list[BinaryIO] = []
            real = os.fdopen

            def fdopen(descriptor: int, mode: str) -> BinaryIO:
                stream = cast("BinaryIO", real(descriptor, mode))
                opened.append(stream)
                return stream

            with (
                patch("security_vm_stage_guest.os.fdopen", side_effect=fdopen),
                self.assertRaises(ValueError),
            ):
                _ = guest.Original(path, 100, 0o600, guest.Control(100))
            self.assertEqual(len(opened), 1)
            self.assertTrue(opened[0].closed)

    def test_wrong_workflow_refuses_before_any_vm_effect(self) -> None:
        """The fixed manual workflow is the only real admission route."""
        with self.assertRaises(ToolError):
            keeper.admit(
                {
                    "GITHUB_ACTIONS": "true",
                    "GITHUB_REF": "refs/heads/main",
                    "GITHUB_WORKFLOW": "another workflow",
                }
            )

    def test_duplicate_json_and_nonfinite_values_refuse(self) -> None:
        """No ambiguous passive record can become a proof projection."""
        for data in (b'{"passed":true,"passed":false}', b'{"n":NaN}'):
            with self.subTest(data=data), self.assertRaises(ValueError):
                _ = guest.record(data)

    def test_original_fixture_signal_is_latched_during_settlement(self) -> None:
        """Final status must adopt a late signal even if cleanup checks permit it."""
        control = guest.Control(100)
        control.settling = True
        control.signal(signal.SIGTERM, None)
        control.check()
        self.assertEqual(control.status, 143)

    def test_playbook_does_not_run_old_stage_or_deployment(self) -> None:
        """New fixed Docker setup composes existing policy without operational cutover."""
        text = (ROOT / "ops/ansible/security-vm-stage.yml").read_text()
        self.assertIn("import_tasks: tasks/docker.yml", text)
        for forbidden in (
            "import_playbook: site.yml",
            "import_playbook: public.yml",
            "release-public.py",
            "upgrade: dist",
        ):
            self.assertNotIn(forbidden, text)

    def test_both_graph_ids_and_ordered_rootfs_are_bound_to_real_archive(self) -> None:
        """Actual mature OCI parsing accepts only config and selected image-manifest."""
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "image.tar"
            image_fixture.image_archive(path, modern=True)
            manifest = image_fixture.release_manifest(path)
            identities, config = guest.archive_graph(path, manifest)
            self.assertEqual(len(identities), 2)
            row: dict[str, object] = {
                "Os": "linux",
                "Architecture": "amd64",
                "Config": {
                    "User": "10001:10001",
                    "Cmd": ["/app/simplestChat"],
                    "Entrypoint": None,
                    "WorkingDir": "/app",
                    "Labels": {"org.opencontainers.image.revision": REVISION},
                    "Env": ["PATH=/usr/bin:/bin"],
                },
                "RootFS": {
                    "Type": "layers",
                    "Layers": cast("dict[str, object]", config["rootfs"])["diff_ids"],
                },
            }
            for identity in identities:
                self.assertEqual(
                    guest.image_profile([dict(row, Id=identity)], identities, config, REVISION),
                    identity,
                )
            for bad in ("sha256:" + "0" * 64, "sha256:" + "f" * 64):
                with self.assertRaises(ValueError):
                    _ = guest.image_profile([dict(row, Id=bad)], identities, config, REVISION)
            changed = copy.deepcopy(row)
            changed["RootFS"] = {"Type": "layers", "Layers": ["sha256:" + "0" * 64]}
            with self.assertRaises(ValueError):
                _ = guest.image_profile(
                    [dict(changed, Id=next(iter(identities)))], identities, config, REVISION
                )

    def test_fd_adoption_closes_original_and_keeps_first_failure(self) -> None:
        """A paired adoption/close fault closes a real descriptor before assertions."""
        body = OSError("INERT_BODY")
        cleanup = OSError("INERT_CLOSE")
        real_close = os.close
        observed: BaseException | None = None
        with tempfile.TemporaryDirectory() as temporary:
            descriptor = os.open(
                Path(temporary) / "owned", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600
            )

            def close_and_fail(number: int) -> None:
                real_close(number)
                raise cleanup

            with (
                patch("security_vm_stage_guest.os.fdopen", side_effect=body),
                patch("security_vm_stage_guest.os.close", side_effect=close_and_fail),
            ):
                try:
                    _ = guest.adopt(descriptor, "wb")
                except OSError as error:
                    observed = error
            self.assertIs(observed, body)
            with self.assertRaises(OSError):
                _ = os.fstat(descriptor)

    def test_probe_body_error_survives_checked_exact_remove_error(self) -> None:
        """Both known probe removal and body ownership survive paired failures."""
        for fail_body in (True, False):
            self._probe_trial(fail_body=fail_body)

    def _probe_trial(self, *, fail_body: bool) -> None:
        """Settle all fake daemon roles before asserting their first-cause identity."""
        events: list[list[str]] = []
        body, cleanup = ValueError("INERT_BODY"), OSError("INERT_REMOVE")
        name = "scpub-stage-proof-" + TOKEN
        inspection: list[dict[str, object]] = [
            {
                "Id": ACTOR,
                "Name": "/" + name,
                "Image": IMAGE,
                "Config": {
                    "Labels": {"simplestchat.stage-proof": TOKEN},
                    "User": "10001:10001",
                    "Entrypoint": ["/usr/bin/timeout"],
                    "Cmd": [
                        "--signal=TERM",
                        "--kill-after=2s",
                        "10s",
                        "/bin/sh",
                        "-c",
                        'for file in /app/migrations/*.sql; do sha384sum "$file" || exit; done',
                    ],
                },
                "HostConfig": {
                    "Init": True,
                    "NetworkMode": "none",
                    "ReadonlyRootfs": True,
                    "Privileged": False,
                    "CapDrop": ["ALL"],
                    "CapAdd": None,
                    "SecurityOpt": ["no-new-privileges"],
                    "Memory": 128 * 1024**2,
                    "NanoCpus": 500000000,
                    "PidsLimit": 32,
                    "LogConfig": {
                        "Type": "local",
                        "Config": {"max-size": "1m", "max-file": "1", "compress": "false"},
                    },
                },
                "Mounts": [],
                "State": {"Running": False, "OOMKilled": False, "ExitCode": 0, "Error": ""},
            }
        ]
        sha = hashlib.sha384(b"INERT").hexdigest()

        @final
        class ProbeRunner:
            def __init__(self) -> None:
                self.control = guest.Control(100)

            def command(
                self, arguments: Sequence[str], *, timeout: float = 30
            ) -> tuple[int, bytes]:
                _ = timeout
                events.append(list(arguments))
                if arguments[0] == "create":
                    return 0, ACTOR.encode()
                if arguments[0] == "inspect":
                    return 0, encoded(inspection)
                if arguments[0] == "wait":
                    return 0, b"0\n"
                if arguments[0] == "logs":
                    if fail_body:
                        raise body
                    return 0, (sha + "  /app/migrations/1_inert.sql\n").encode()
                if arguments[0] == "rm":
                    raise cleanup
                return 0, b""

        runner = ProbeRunner()
        observed: BaseException | None = None
        try:
            _ = guest.separate_probe(
                cast("guest.Runner", cast("object", runner)), IMAGE, {"1": sha}, TOKEN
            )
        except (ValueError, OSError) as error:
            observed = error
        self.assertIs(observed, body if fail_body else cleanup)
        self.assertIn(["rm", "--force", ACTOR], events)
        self.assertFalse(runner.control.settling)

    def test_metadata_uses_original_gh_and_exact_held_zip_before_cached_crypto(self) -> None:
        """Two fixed real-role adapters cannot drift to a PATH shadow or wrong attempt/digest."""
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary).resolve()
            zip_path, gh_path = directory / "artifact.zip", directory / "gh-original"
            _ = zip_path.write_bytes(b"INERT_ZIP")
            _ = gh_path.write_bytes(b"INERT_TOOL_NOT_EXECUTED")
            zip_path.chmod(0o600)
            gh_path.chmod(0o700)
            control = fetch_vm.Control()
            held_zip = fetch_vm.Held.open(zip_path, 100, control)
            held_gh = fetch_vm.Held.open(gh_path, 100, control)
            calls: list[list[str]] = []
            artifact = {
                "id": 17,
                "expired": False,
                "name": "simplestchat-production-" + REVISION,
                "size_in_bytes": held_zip.size,
                "digest": "sha256:" + held_zip.sha256,
                "workflow_run": {
                    "id": 42,
                    "head_sha": REVISION,
                    "repository_id": 8,
                    "head_repository_id": 8,
                },
            }
            run = {
                "id": 42,
                "run_attempt": 1,
                "head_branch": "main",
                "head_sha": REVISION,
                "name": "CI",
                "path": ".github/workflows/ci.yml",
                "event": "push",
                "status": "completed",
                "conclusion": "success",
                "repository": {"full_name": fetch_vm.REPOSITORY},
                "head_repository": {"full_name": fetch_vm.REPOSITORY},
            }

            @final
            class ApiHost:
                def __init__(self) -> None:
                    self.control, self.fixed_tools = control, {"gh": held_gh}

                def command(
                    self,
                    name: str,
                    arguments: Sequence[str],
                    environment: Mapping[str, str],
                    timeout: float,
                    accepted: tuple[int, ...],
                ) -> tuple[int, bytes]:
                    _ = environment, timeout, accepted
                    calls.append(list(arguments))
                    return 0, encoded(artifact if name.endswith("artifact") else run)

            @final
            class Tools:
                def check(self, current: fetch_vm.Control) -> None:
                    held_gh.check(current)

            host, tools = ApiHost(), Tools()
            selected = {"revision": REVISION, "artifactId": 17, "ciRunId": 42}
            metadata: dict[str, object] = {}
            try:
                metadata = keeper.current_server_metadata(
                    cast("receive.Host", cast("object", host)),
                    cast("fetch_vm.NativeTools", cast("object", tools)),
                    selected,
                    1,
                    {"artifact.zip": held_zip},
                    {"PATH": "/INERT_SHADOW", "GH_TOKEN": "INERT"},
                )
                artifact["digest"] = "sha256:" + "0" * 64
                with self.assertRaises(ToolError):
                    _ = keeper.current_server_metadata(
                        cast("receive.Host", cast("object", host)),
                        cast("fetch_vm.NativeTools", cast("object", tools)),
                        selected,
                        1,
                        {"artifact.zip": held_zip},
                        {"PATH": "/INERT_SHADOW"},
                    )
            finally:
                held_zip.close()
                held_gh.close()
            self.assertEqual(metadata["runAttempt"], 1)
            self.assertEqual(metadata["zipSha256"], hashlib.sha256(b"INERT_ZIP").hexdigest())
            self.assertEqual(len(calls), 4)
            self.assertTrue(
                all(
                    row[0] == str(gh_path) and row[1:4] == ["api", "--hostname", "github.com"]
                    for row in calls
                )
            )

    def test_real_consumed_inert_nonzero_child_is_never_signalled_after_reap(self) -> None:
        """Actual child/pipe/log completion precedes a negative status assertion."""
        opened: list[BinaryIO] = []
        original_adopt = guest.adopt

        def adopt(descriptor: int, mode: str) -> BinaryIO:
            stream = original_adopt(descriptor, mode)
            opened.append(stream)
            return stream

        observed: BaseException | None = None
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            runner = guest.Runner(guest.Control(100), [])
            with (
                patch.object(guest, "ROOT", root),
                patch.object(guest, "DOCKER", (sys.executable, "-I", "-B", "-c")),
                patch.object(guest, "adopt", side_effect=adopt),
                patch("security_vm_stage_guest.bounded_process.stop") as stopped,
            ):
                try:
                    _ = runner.command(
                        [
                            (
                                'import sys;sys.stdout.write("INERT\\n");'
                                'sys.stderr.write("INERT\\n");sys.exit(7)'
                            )
                        ]
                    )
                except ValueError as error:
                    observed = error
            self.assertIsNotNone(observed)
            self.assertEqual(runner.last_status, 7)
            self.assertEqual(stopped.call_count, 0)
            self.assertEqual(len(opened), 2)
            self.assertTrue(all(stream.closed for stream in opened))

    def test_controller_main_late_restore_signal_invalidates_earlier_positive_bytes(self) -> None:
        """Exercise actual main restoration/final status in this owned unittest process."""
        observed_control: list[fetch_vm.Control] = []
        calls = 0
        output = io.BytesIO()
        data = b""

        def run(options: receive.Options, control: fetch_vm.Control) -> dict[str, object]:
            options.output.mkdir(mode=0o700)
            observed_control.append(control)
            return {"passed": True, "cleanupPassed": True}

        def handler(number: int, callback: object) -> int:
            nonlocal calls
            _ = number, callback
            calls += 1
            if calls == FIRST_RESTORE_CALL:
                observed_control[0].signal(signal.SIGINT, None)
            return 0

        status = 1
        try:
            with tempfile.TemporaryDirectory() as temporary:
                destination = Path(temporary).resolve() / "output"
                argv = [
                    "stage-fixture",
                    "--run-id",
                    "42",
                    "--run-attempt",
                    "1",
                    "--artifact-id",
                    "17",
                    "--output",
                    str(destination),
                ]
                with (
                    patch.object(keeper, "run", side_effect=run),
                    patch("security_vm_stage.sys.argv", argv),
                    patch("security_vm_stage.signal.signal", side_effect=handler),
                    patch("security_vm_stage.sys.stdout", SimpleNamespace(buffer=output)),
                ):
                    status = keeper.main()
                data = output.getvalue()
        finally:
            output.close()
        self.assertEqual(status, 130)
        self.assertEqual(json.loads(data), {"passed": True, "cleanupPassed": True})
        self.assertEqual(calls, 4)

    def test_server_tree_expiry_and_signal_refuse_before_open(self) -> None:
        """No directory FD is admitted after either original controller fence."""
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary).resolve() / "not-opened"
            for cancelled in (False, True):
                control = fetch_vm.Control()
                if cancelled:
                    control.status = 143
                else:
                    control.started -= 2000
                with patch("security_vm_stage.os.open") as opened, self.assertRaises(ToolError):
                    _ = keeper.ServerTree(path, control)
                opened.assert_not_called()

    def test_guest_original_expiry_and_signal_refuse_before_open(self) -> None:
        """No original file or stream is admitted after the nested original fence."""
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary).resolve() / "not-opened"
            for cancelled in (False, True):
                control = guest.Control(100)
                if cancelled:
                    control.status = 130
                else:
                    control.body_end = control.started - 1
                with (
                    patch("security_vm_stage_guest.os.open") as opened,
                    patch.object(guest, "adopt") as adopted,
                    self.assertRaises(ValueError),
                ):
                    _ = guest.Original(path, 100, 0o600, control)
                opened.assert_not_called()
                adopted.assert_not_called()

    def test_evidence_body_survives_close(  # noqa: C901 -- Settle paired faults before assertions.
        self,
    ) -> None:
        """An actual admitted wrong-mode stream closes before its first-error assertion."""
        cleanup = OSError("INERT_EVIDENCE_CLOSE")
        guards: list[ValueError] = []
        opened: list[BinaryIO] = []
        actual_require, actual_adopt = guest.require, guest.adopt
        observed: BaseException | None = None
        closed_before_fallback: list[bool] = []

        def require(value: object) -> None:
            try:
                actual_require(value)
            except ValueError as error:
                guards.append(error)
                raise

        @final
        class CloseFault:
            def __init__(self, stream: BinaryIO) -> None:
                self.stream = stream

            def fileno(self) -> int:
                return self.stream.fileno()

            def read(self, size: int) -> bytes:
                return self.stream.read(size)

            def close(self) -> None:
                self.stream.close()
                raise cleanup

            def __enter__(self) -> CloseFault:
                return self

            def __exit__(
                self,
                kind: type[BaseException] | None,
                error: BaseException | None,
                trace: TracebackType | None,
            ) -> None:
                _ = kind, error, trace
                self.close()

        def adopt(descriptor: int, mode: str) -> BinaryIO:
            stream = actual_adopt(descriptor, mode)
            opened.append(stream)
            return cast("BinaryIO", cast("object", CloseFault(stream)))

        try:
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                public = root / "public"
                (public / "results").mkdir(parents=True)
                path = root / "stage-observer-001.stdout"
                descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                try:
                    _ = os.write(descriptor, b"INERT_PRIVATE_EVIDENCE")
                    os.fchmod(descriptor, 0o644)
                finally:
                    os.close(descriptor)
                marker = ((TOKEN + "\n").encode(), path.stat())
                with (
                    patch.object(guest, "ROOT", root),
                    patch.object(guest, "PUBLIC", public),
                    patch.object(guest, "require", side_effect=require),
                    patch.object(guest, "adopt", side_effect=adopt),
                    patch("security_vm_stage_guest.os.geteuid", return_value=0),
                    patch("security_vm_ops_guest.read_fixed", return_value=marker),
                ):
                    try:
                        _ = guest.private_evidence(TOKEN)
                    except (ValueError, OSError) as error:
                        observed = error
                closed_before_fallback = [stream.closed for stream in opened]
        finally:
            for stream in opened:
                if not stream.closed:
                    stream.close()
        self.assertEqual(len(guards), 1)
        self.assertIs(observed, guards[0])
        self.assertIsNot(observed, cleanup)
        self.assertEqual(closed_before_fallback, [True])


if __name__ == "__main__":
    _ = unittest.main()
