"""Validate disposable-VM orchestration offline, never substituting mocks for guest evidence."""

from __future__ import annotations

import errno
import hashlib
import json
import os
import platform
import selectors
import shlex
import sys
import tempfile
import time
import unittest
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, cast, override
from unittest.mock import patch

if TYPE_CHECKING:
    from collections.abc import Sequence

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "build"))

import security_vm as vm
from release_artifact import Manifest
from security_findings import object_value
from security_tools import ToolError

# isort: split
# The security modules install the canonical standalone operations-helper directory.
import bounded_process

RUN = "1" * 32
REVISION = "2" * 40
GOOD_RECAP = (
    b"PLAY RECAP ***\nvmfixture : ok=29 changed=0 unreachable=0 failed=0"
    + b" skipped=4 rescued=0 ignored=0\n"
)


@dataclass
class RecordedHost(vm.Host):
    """Capture reviewed argv without invoking an installer, network client or VM."""

    calls: list[tuple[str, list[str]]] = field(default_factory=list, init=False)

    @override
    def __post_init__(self) -> None:
        """Use no ambient process environment for these pure orchestration fixtures."""
        self.calls = []

    @override
    def run(
        self,
        name: str,
        argv: Sequence[str],
        *,
        timeout: float = 120,
        accepted: tuple[int, ...] = (0,),
    ) -> tuple[int, bytes]:
        """Provide only modeled ordinary command outputs; unknown paths fail."""
        self.calls.append((name, list(argv)))
        if name.startswith("key-"):
            path = Path(argv[-1])
            _ = path.write_text("fixture private key; never a real credential\n")
            path.chmod(0o600)
            _ = path.with_suffix(".pub").write_text("ssh-ed25519 YWJj vm-fixture\n")
            return 0, b""
        if name == "cloud-seed":
            return 0, b""
        if name == "source-revision":
            return 0, (REVISION + "\n").encode()
        if name == "source-clean":
            return 0, b""
        if name.endswith(("-first", "-second")):
            return 0, GOOD_RECAP
        message = "Unmodeled command"
        raise AssertionError(message)


def fixture_executable(name: str) -> str:
    """Resolve only modeled executable paths without consulting the host installation."""
    return "/usr/bin/" + name


def object_json(data: str | bytes) -> dict[str, object]:
    """Validate fixture JSON objects without propagating the decoder's dynamic type."""
    return object_value(cast("object", json.loads(data)))


@dataclass
class CleanupFailure:
    """Model an exact cleanup failure without starting or inspecting any real VM."""

    calls: int = 0
    failure: str | None = None

    def close(self) -> None:
        """Count the single cleanup attempt and keep failure visible."""
        self.calls += 1
        message = "fixture_cleanup_failure"
        raise ToolError(message)

    def diagnostics(self) -> dict[str, object]:
        """Supply only modeled metadata when exact cleanup fails."""
        return {"terminalBeforeCleanup": {"state": "running"}, "cleanupReturnCode": None}


@dataclass
class FixtureObserver:
    """Model the diagnostic-only observer for inert processes that do not speak QMP."""

    failure: str | None = None

    def start(self) -> None:
        """Start no real socket; independent QMP tests exercise the actual protocol."""

    def close(self) -> None:
        """Own no fixture resources beyond the real child already checked by Guest."""

    def diagnostics(self) -> dict[str, object]:
        """Make modeled observer evidence explicit in orchestration-only test receipts."""
        return {"fixture": True}


def manifest() -> Manifest:
    """Build a complete current release contract without representing a usable image."""
    return Manifest(
        schemaVersion=1,
        revision=REVISION,
        platform="linux/amd64",
        archiveSha256="3" * 64,
        imageTag="simplestchat-release/production:" + REVISION,
        migrations={"1": "4" * 96},
        createdAt="2026-09-30T00:00:00Z",
    )


class InputTests(unittest.TestCase):
    """Only explicit local resources and the reviewed immutable Debian bytes are accepted."""

    def test_published_pin_has_fixed_release_and_full_checksum(self) -> None:
        """The maintained pin has no latest alias or alternate host."""
        selected = vm.CloudImage.read()
        self.assertIn("/20260914-2601/", selected.url)
        self.assertEqual(len(selected.sha512), 128)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "pin.json"
            raw = object_json(vm.PIN.read_text())
            for changes in (
                {"schemaVersion": True},
                {"url": "https://example.test/disk.qcow2"},
                {"url": selected.url.replace("20260914-2601/", "latest/")},
                {"sha512": "a" * 127},
                {"architecture": "arm64"},
            ):
                with self.subTest(changes=changes):
                    _ = path.write_text(json.dumps({**raw, **changes}))
                    with self.assertRaises(ToolError):
                        _ = vm.CloudImage.read(path)

    def test_image_authentication_rejects_changed_bytes_links_and_oversize(self) -> None:
        """Only the exact bounded regular bytes reach QEMU's image parser."""
        content = b"ordinary image fixture bytes"
        expected = hashlib.sha512(content).hexdigest()
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "image"
            _ = path.write_bytes(content)
            vm.verify_image(path, expected)
            with self.assertRaisesRegex(ToolError, "vm_image_digest"):
                vm.verify_image(path, "0" * 128)
            with patch.object(vm, "MAX_IMAGE", 4), self.assertRaises(ToolError):
                vm.verify_image(path, expected)
            link = path.with_suffix(".link")
            link.symlink_to(path)
            with self.assertRaises(OSError):
                vm.verify_image(link, expected)

    def test_unsupported_host_is_failure_evidence_not_a_mocked_guest_pass(self) -> None:
        """A Mac cannot claim Linux/KVM execution and does not launch a child."""
        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.object(platform, "system", return_value="Darwin"),
            patch.object(vm, "Guest") as guest,
        ):
            root = Path(temporary).resolve()
            output = root / "result"
            with self.assertRaisesRegex(ToolError, "vm_requires_linux_amd64"):
                _ = vm.run(root / "artifact", output)
            report = object_json((output / "summary.json").read_bytes())
            self.assertFalse(report["passed"])
            self.assertFalse(report["guestExecuted"])
            self.assertTrue(report["cleanupPassed"])
            guest.assert_not_called()

    def test_release_identity_is_bound_before_guest_creation(self) -> None:
        """Canonical artifact validation and exact checkout revision are both required."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            artifact = root / "artifact"
            artifact.mkdir(mode=0o700)
            _ = (artifact / "release.json").write_text(json.dumps(manifest()))
            _ = (artifact / "image.tar").write_bytes(b"not a real image")
            host = RecordedHost(root, time.monotonic() + 30)
            with patch.object(vm, "verify_archive", return_value={}) as verify:
                self.assertEqual(vm.artifact_input(host, artifact)["revision"], REVISION)
                verify.assert_called_once()
            changed = manifest()
            changed["revision"] = "5" * 40
            changed["imageTag"] = "simplestchat-release/production:" + "5" * 40
            _ = (artifact / "release.json").write_text(json.dumps(changed))
            with (
                patch.object(vm, "verify_archive", return_value={}),
                self.assertRaisesRegex(ToolError, "vm_release_revision_differs"),
            ):
                _ = vm.artifact_input(host, artifact)


class BoundaryTests(unittest.TestCase):
    """Inspect exact target, resource and idempotence contracts using fixed local data."""

    def test_qemu_has_finite_limits_and_only_loopback_ssh_forwarding(self) -> None:
        """No host socket, provider, bridged networking or daemonized process is selected."""
        work = Path("/owned/fixture,with comma")
        with patch.object(vm, "executable", side_effect=fixture_executable):
            args = vm.qemu_command(work, 22345, RUN)
        self.assertEqual(args[0], "/usr/bin/timeout")
        self.assertIn("1800s", args)
        self.assertIn("--as=5368709120", args)
        self.assertIn("--fsize=18253611008", args)
        self.assertIn("--core=0", args)
        for option, value in (
            ("-machine", "q35,accel=kvm"),
            ("-smp", "2"),
            ("-m", "3072"),
            ("-monitor", "none"),
        ):
            self.assertEqual(args[args.index(option) + 1], value)
        self.assertEqual(
            args[args.index("-netdev") + 1], "user,id=net0,hostfwd=tcp:127.0.0.1:22345-:22"
        )
        self.assertEqual(
            args[args.index("-qmp") + 1],
            "unix:/owned/fixture,,with comma/qmp.sock,server=on,wait=on",
        )
        self.assertIn("-no-reboot", args)
        disks = [
            object_json(args[index + 1]) for index, item in enumerate(args) if item == "-blockdev"
        ]
        self.assertEqual(object_value(disks[0]["file"])["filename"], str(work / "guest.qcow2"))
        self.assertTrue(disks[1]["read-only"])
        self.assertNotIn("-daemonize", args)
        self.assertNotIn("-virtfs", args)
        self.assertNotIn("docker.sock", " ".join(args))

    def test_generated_inventory_and_ssh_cannot_select_an_external_host(self) -> None:
        """Ansible receives the same strict generated host-key policy as direct SSH."""
        work = Path("/owned/space in path")
        inventory = vm.fixture_inventory(work, 22345, RUN, Path("/artifact"), REVISION)
        group = object_value(inventory["benchmark_hosts"])
        hosts = object_value(group["hosts"])
        self.assertEqual(set(hosts), {"vmfixture"})
        host = object_value(hosts["vmfixture"])
        self.assertEqual(host["ansible_host"], "127.0.0.1")
        self.assertEqual(host["ansible_port"], 22345)
        self.assertEqual(shlex.split(cast("str", host["ansible_ssh_args"])), vm.ssh_options(work))
        with patch.object(vm, "executable", return_value="/usr/bin/ssh"):
            args = vm.ssh_command(work, 22345, ["printf", "literal; value"])
        self.assertIn("StrictHostKeyChecking=yes", args)
        self.assertIn("IdentityAgent=none", args)
        self.assertIn("ProxyCommand=none", args)
        self.assertIn("ControlMaster=no", args)
        self.assertEqual(args[-2], "fixture@127.0.0.1")
        self.assertEqual(shlex.split(args[-1]), ["printf", "literal; value"])
        with self.assertRaises(ToolError):
            _ = vm.fixture_inventory(work, 22, RUN, Path("/artifact"), REVISION)

    def test_cloud_seed_contains_only_fixture_keys_and_exact_marker(self) -> None:
        """The host-key verifier and seeded key originate from the same owned inputs."""
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            host = RecordedHost(work, time.monotonic() + 30)
            with patch.object(vm, "executable", side_effect=fixture_executable):
                vm.create_seed(host, work, 22345, RUN)
            text = (work / "user-data").read_text().removeprefix("#cloud-config\n")
            config = object_json(text)
            self.assertFalse(config["ssh_pwauth"])
            self.assertTrue(config["disable_root"])
            self.assertEqual(config["ssh_genkeytypes"], [])
            self.assertIn("[127.0.0.1]:22345 ssh-ed25519", (work / "known_hosts").read_text())
            self.assertIn(RUN, text)
            self.assertEqual((work / "user-data").stat().st_mode & 0o777, 0o600)
            self.assertEqual(
                [name for name, _ in host.calls], ["key-client", "key-host", "cloud-seed"]
            )

    def test_recap_rejects_changed_second_apply_or_incomplete_execution(self) -> None:
        """Success requires the single expected host and no rescued, ignored or failed task."""
        self.assertEqual(vm.recap(GOOD_RECAP, unchanged=True)["ok"], 29)
        self.assertEqual(
            vm.recap(GOOD_RECAP.replace(b"changed=0", b"changed=12"), unchanged=False)["changed"],
            12,
        )
        cases = [b"", GOOD_RECAP + GOOD_RECAP, GOOD_RECAP.replace(b"changed=0", b"changed=1")]
        cases.extend(
            GOOD_RECAP.replace(name + b"=0", name + b"=1")
            for name in (b"failed", b"unreachable", b"rescued", b"ignored")
        )
        cases.append(GOOD_RECAP + GOOD_RECAP.replace(b"vmfixture", b"otherhost"))
        for content in cases:
            with self.subTest(content=content), self.assertRaises(ToolError):
                _ = vm.recap(content, unchanged=True)

    def test_playbook_uses_real_configurations_without_source_clone_or_host_override(self) -> None:
        """Only the selected configuration tags run, and the second apply is enforced."""
        host = RecordedHost(Path("/unused"), time.monotonic() + 30)
        result = vm.playbook(host, Path("/owned/inventory.json"), "site", second=True)
        self.assertEqual(result["changed"], 0)
        self.assertEqual(host.calls[0][1][-2:], ["--tags", "host,docker,benchmark"])
        self.assertIn("ops/ansible/site.yml", host.calls[0][1])
        self.assertEqual(host.calls[0][1][host.calls[0][1].index("--limit") + 1], "vmfixture")

    def test_guest_receipts_keep_only_validated_aggregate_evidence(self) -> None:
        """A matching action alone cannot conceal failed restore checks or unknown data."""
        host = RecordedHost(Path("/unused"), time.monotonic() + 30)
        counts = {
            "users": 1,
            "rooms": 1,
            "sessions": 0,
            "credentials": 0,
            "activeIncidents": 1,
            "resolvedIncidents": 1,
        }
        base: dict[str, object] = {
            "schemaVersion": 1,
            "runId": RUN,
            "action": "backup-restore",
            "passed": True,
            "archiveValidated": True,
            "restoreVerified": True,
            "cleanupPassed": True,
            "counts": counts,
        }
        with patch.object(RecordedHost, "run", return_value=(0, json.dumps(base).encode())):
            result = vm.guest_action(host, Path("/owned"), 22345, RUN, "backup-restore")
        self.assertEqual(result["counts"], counts)
        self.assertTrue(result["cleanupPassed"])
        self.assertNotIn("runId", result)
        for changes in (
            {"schemaVersion": True},
            {"runId": "other"},
            {"cleanupPassed": False},
            {"counts": {**counts, "sessions": True}},
            {"unreviewed": "data"},
        ):
            with (
                self.subTest(changes=changes),
                patch.object(
                    RecordedHost, "run", return_value=(0, json.dumps({**base, **changes}).encode())
                ),
                self.assertRaises(ToolError),
            ):
                _ = vm.guest_action(host, Path("/owned"), 22345, RUN, "backup-restore")


class StartupDiagnosticTests(unittest.TestCase):
    """Recognize actionable emulator failures without publishing any arbitrary input words."""

    def test_common_startup_errors_keep_only_fixed_classes_reasons_and_components(self) -> None:
        """KVM, block, resource, GLib/thread and sandbox failures remain distinguishable."""
        for line, expected in (
            ("qemu: failed to initialize KVM: Permission denied", "kvm"),
            ("qemu: backing file format not specified for rootdisk", "backing_format"),
            ("qemu: seccomp sandbox: Operation not permitted", "sandbox"),
            ("qemu: No bootable device for q35", "boot_device"),
            ("qemu: cannot allocate memory for pc.ram", "memory"),
            ("prlimit: failed to set RLIMIT_AS: Invalid argument", "resource_limit"),
            (
                "(process:123): GLib-ERROR **: creating thread 'gmain': "
                + "Resource temporarily unavailable",
                "thread_start",
            ),
            ("qemu: pthread_create failed: Resource temporarily unavailable", "thread_start"),
        ):
            with self.subTest(expected=expected):
                result = vm.startup_errors((line + " private-path-fixture\n").encode())
                encoded = json.dumps(result)
                self.assertIn(expected, encoded)
                self.assertNotIn("private-path-fixture", encoded)
                self.assertNotIn("gmain", encoded)
                self.assertEqual(result["withheldLines"], 0)
        result = vm.startup_errors(
            b"qemu: kvm \x1b[31mprivate-fixture\n"
            + b"arbitrary-private-fixture\n"
            + b"qemu: kvm "
            + b"x" * vm.MAX_STARTUP_LINE
            + b"\n"
        )
        self.assertEqual(result, {"messages": [], "withheldLines": 3})
        result = vm.startup_errors(b"qemu: kvm\n" * (vm.MAX_STARTUP_LINES + 1))
        self.assertEqual(result["withheldLines"], 1)


class LifecycleTests(unittest.TestCase):
    """Use only tiny owned Python processes to check exact cleanup and stream limits."""

    def test_guest_output_and_exact_group_cleanup_keep_the_leader_unreaped(self) -> None:
        """The drainer never reaps the leader before the canonical cleanup owns its PGID."""
        with tempfile.TemporaryDirectory() as temporary:
            host = vm.Host(Path(temporary), time.monotonic() + 30)
            guest = vm.Guest(host, [sys.executable, "-c", "print('fixture output',flush=True)"])
            try:
                guest.thread.join(timeout=5)
                self.assertFalse(guest.thread.is_alive())
                self.assertIsNone(guest.child.returncode)
                with self.assertRaisesRegex(ToolError, "vm_child_exited"):
                    guest.require_alive()
                with patch.object(
                    bounded_process, "signal_group", wraps=bounded_process.signal_group
                ) as sent:
                    guest.close()
                for arguments in sent.call_args_list:
                    self.assertEqual(arguments.args[0], guest.child.pid)
                self.assertIsNotNone(guest.child.returncode)
                self.assertEqual((host.output / "qemu.stdout").read_bytes(), b"fixture output\n")
                self.assertEqual(
                    guest.diagnostics()["terminalBeforeCleanup"],
                    {"state": "exited", "exitCode": 0},
                )
            finally:
                guest.close()

    def test_oversized_small_fixture_output_cannot_be_recorded_as_healthy(self) -> None:
        """A finite 100-byte local fixture exceeds the deliberately tiny test budget."""
        with tempfile.TemporaryDirectory() as temporary, patch.object(vm, "MAX_STREAM", 16):
            host = vm.Host(Path(temporary), time.monotonic() + 30)
            guest = vm.Guest(host, [sys.executable, "-c", "print('x'*100,flush=True)"])
            try:
                guest.thread.join(timeout=5)
                self.assertIsNotNone(guest.failure)
                with self.assertRaises(ToolError):
                    guest.require_alive()
                self.assertLessEqual((host.output / "qemu.stdout").stat().st_size, 16)
                self.assertEqual(guest.failure, "vm_stdout_limit")
            finally:
                guest.close()

    def test_startup_stderr_is_bounded_and_never_exposes_private_strings(self) -> None:
        """An actual child's nonzero exit and emulator error retain only fixed public words."""
        content = (
            b"qemu-system-x86_64: /private/fixture-key/guest.qcow2: backing format required\n"
            + b"unknown fixture-secret-value\n"
        )
        with tempfile.TemporaryDirectory() as temporary, patch.object(vm, "MAX_STARTUP_STDERR", 80):
            host = vm.Host(Path(temporary), time.monotonic() + 30)
            guest = vm.Guest(
                host,
                [sys.executable, "-c", f"import os; os.write(2,{content!r}); raise SystemExit(7)"],
            )
            try:
                guest.thread.join(timeout=5)
                guest.close()
                result = guest.diagnostics()
                self.assertEqual(
                    result["terminalBeforeCleanup"], {"state": "exited", "exitCode": 7}
                )
                self.assertEqual(result["cleanupReturnCode"], 7)
                stderr = object_value(result["startupStderr"])
                self.assertEqual(stderr["bytesObserved"], len(content))
                self.assertEqual(stderr["sha256"], hashlib.sha256(content).hexdigest())
                self.assertEqual(stderr["prefixBytes"], 80)
                self.assertTrue(stderr["truncated"])
                public = json.dumps(result)
                self.assertIn("backing_format", public)
                for private in ("/private", "fixture-key", "fixture-secret-value", "unknown"):
                    self.assertNotIn(private, public)
            finally:
                guest.close()

    def test_stderr_overflow_and_drainer_failures_have_distinct_fixed_codes(self) -> None:
        """I/O and unexpected thread failures never serialize their private exception text."""
        for failure, expected, error_number in (
            (OSError(errno.ENOSPC, "private-fixture-key"), "vm_drainer_io_error", errno.ENOSPC),
            (ValueError("private-fixture-key"), "vm_drainer_unexpected_error", None),
        ):
            with (
                self.subTest(expected=expected),
                tempfile.TemporaryDirectory() as temporary,
            ):
                host = vm.Host(Path(temporary), time.monotonic() + 30)
                with patch.object(selectors, "DefaultSelector", side_effect=failure):
                    guest = vm.Guest(host, [sys.executable, "-c", "import time; time.sleep(30)"])
                    guest.thread.join(timeout=5)
                try:
                    guest.close()
                    result = guest.diagnostics()
                    self.assertEqual(result["drainer"], expected)
                    self.assertEqual(result["drainerErrno"], error_number)
                    self.assertNotIn("private-fixture-key", json.dumps(result))
                finally:
                    guest.close()
        with tempfile.TemporaryDirectory() as temporary, patch.object(vm, "MAX_STREAM", 16):
            host = vm.Host(Path(temporary), time.monotonic() + 30)
            guest = vm.Guest(host, [sys.executable, "-c", "import os; os.write(2,b'x'*100)"])
            try:
                guest.thread.join(timeout=5)
                guest.close()
                self.assertEqual(guest.failure, "vm_stderr_limit")
            finally:
                guest.close()

    def test_authenticated_ssh_closes_startup_diagnostics_before_cloud_init(self) -> None:
        """Even a subsequent initialization failure cannot publish authenticated guest output."""
        with tempfile.TemporaryDirectory() as temporary:
            host = vm.Host(Path(temporary), time.monotonic() + 30)
            guest = vm.Guest(host, [sys.executable, "-c", "import time; time.sleep(30)"])
            try:
                guest.startup_capture(b"qemu: kvm fixture-private\n")
                with (
                    patch.object(vm, "executable", side_effect=fixture_executable),
                    patch.object(
                        vm.Host, "run", side_effect=[(0, b""), ToolError("cloud_fixture")]
                    ),
                    self.assertRaisesRegex(ToolError, "cloud_fixture"),
                ):
                    vm.boot(host, guest, Path(temporary), 22345)
                guest.startup_capture(b"qemu: memory fixture-private-after-auth\n")
                guest.close()
                self.assertTrue(guest.authenticated.is_set())
                self.assertEqual(guest.startup_buffer, b"")
                self.assertNotIn("startupStderr", guest.diagnostics())
            finally:
                guest.close()

    def test_late_output_during_cleanup_cannot_publish_success(self) -> None:
        """A real owned child exceeds its stream budget only after the final liveness check."""
        with tempfile.TemporaryDirectory() as temporary, patch.object(vm, "MAX_STREAM", 16):
            root = Path(temporary).resolve()
            ready = root / "ready"
            script = (
                "import os,signal,sys,time,pathlib\n"
                "def stop(*args):\n    os.write(1,b'x'*100)\n    sys.exit(0)\n"
                "signal.signal(signal.SIGTERM,stop)\n"
                "pathlib.Path(sys.argv[1]).touch()\n"
                "time.sleep(30)\n"
            )

            def boot_ready(_host: vm.Host, guest: vm.Guest, _work: Path, _port: int) -> None:
                """Wait at most five seconds for the inert fixture's installed signal handler."""
                for _ in range(500):
                    if ready.exists():
                        guest.require_alive()
                        return
                    time.sleep(0.01)
                self.fail("owned fixture readiness timeout")

            with (
                patch.object(vm, "preflight"),
                patch.object(vm, "artifact_input", return_value=manifest()),
                patch.object(vm, "prepare_disk"),
                patch.object(vm, "create_seed"),
                patch.object(
                    vm, "qemu_command", return_value=[sys.executable, "-c", script, str(ready)]
                ),
                patch.object(vm, "boot", side_effect=boot_ready),
                patch.object(vm, "exercise", return_value={}),
                patch.object(vm, "Observer", return_value=FixtureObserver()),
                self.assertRaisesRegex(ToolError, "vm_stdout_limit"),
            ):
                _ = vm.run(root / "artifact", root / "output")
            result = object_json((root / "output/summary.json").read_bytes())
            self.assertFalse(result["passed"])
            self.assertTrue(result["cleanupPassed"])
            self.assertEqual(result["failure"], "vm_stdout_limit")
            lifecycle = object_value(result["vmLifecycle"])
            self.assertEqual(lifecycle["drainer"], "vm_stdout_limit")
            self.assertEqual(lifecycle["cleanupReturnCode"], 0)
            self.assertEqual(lifecycle["terminalBeforeCleanup"], {"state": "running"})
            self.assertFalse(list((root / "output").glob("vm-private-*")))

    def test_cancellation_or_cleanup_failure_cannot_publish_success(self) -> None:
        """Even completed modeled guest checks fail when owned cleanup cannot be confirmed."""
        guest = CleanupFailure()
        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.object(vm, "preflight"),
            patch.object(vm, "artifact_input", return_value=manifest()),
            patch.object(vm, "prepare_disk"),
            patch.object(vm, "create_seed"),
            patch.object(vm, "qemu_command", return_value=["never-executed"]),
            patch.object(vm, "boot"),
            patch.object(vm, "exercise", return_value={}),
            patch.object(vm, "Guest", return_value=guest),
        ):
            root = Path(temporary).resolve()
            with self.assertRaises(ToolError):
                _ = vm.run(root / "artifact", root / "output")
            report = object_json((root / "output/summary.json").read_bytes())
            self.assertFalse(report["passed"])
            self.assertFalse(report["cleanupPassed"])
            self.assertTrue(list((root / "output").glob("vm-private-*")))
            self.assertEqual(guest.calls, 1)

    def test_command_environment_omits_credentials_but_retains_job_cleanup_identity(self) -> None:
        """GitHub can reap this job's descendants even after an uncatchable controller stop."""
        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.dict(
                os.environ,
                {
                    "SSH_AUTH_SOCK": "/not-forwarded",
                    "GITHUB_TOKEN": "fixture-only",
                    "ANSIBLE_LOG_PATH": "/not-forwarded",
                    "RUNNER_TRACKING_ID": "github-job-fixture",
                },
            ),
        ):
            host = vm.Host(Path(temporary), time.monotonic() + 30)
            self.assertEqual(host.env["RUNNER_TRACKING_ID"], "github-job-fixture")
            self.assertNotIn("SSH_AUTH_SOCK", host.env)
            self.assertNotIn("GITHUB_TOKEN", host.env)
            self.assertNotIn("ANSIBLE_LOG_PATH", host.env)


if __name__ == "__main__":
    _ = unittest.main()
