"""Exercise guest ownership and assertion failures without claiming a live VM run."""

from __future__ import annotations

import io
import json
import os
import platform
import shutil
import sys
import tempfile
import time
import unittest
from copy import deepcopy
from pathlib import Path
from typing import TYPE_CHECKING, Unpack
from unittest.mock import patch

from test_support import ROOT, objects, strings, yaml_value

# isort: split
import bounded_process
import release_public as release
import security_vm_guest as guest
import test_public_templates as templates
from release_json import decode_json, object_value, string_value

if TYPE_CHECKING:
    from collections.abc import Sequence

    from release_json import JsonObject, JsonValue

_ = ROOT  # Import the test bootstrap before the flat installed-helper modules.

RUN_ID = "a" * 32
REVISION = "b" * 40
CONTAINER = "c" * 64
IMAGE = "sha256:" + "d" * 64


class RecordedRunner:
    """Model only bounded inspection and owned cleanup; never invoke a process."""

    def __init__(self) -> None:
        """Start with one fixed maintenance identity and an empty command ledger."""
        self.attempt: Path = Path("/unused-fixture-evidence")
        self.commands: list[tuple[str, ...]] = []
        self.present: bool = True
        self.metadata: JsonObject = runtime_container()
        self.output: bytes = b""

    def run(self, args: Sequence[str], **options: Unpack[release.CommandOptions]) -> bytes:
        """Retain argv for policy assertions and return explicit property fixture bytes."""
        _ = options
        self.commands.append(tuple(args))
        return self.output

    def docker(self, *args: str, **options: Unpack[release.CommandOptions]) -> bytes:
        """Reject any unmodelled daemon operation instead of silently accepting it."""
        _ = options
        self.commands.append(args)
        if args[:2] == ("inspect", "--format") and args[-1] == CONTAINER:
            return json.dumps(self.metadata).encode()
        if args == ("stop", "--time", "10", CONTAINER):
            return b""
        if args == ("rm", CONTAINER):
            self.present = False
            return b""
        message = "Unmodelled fixture Docker operation"
        raise AssertionError(message)

    def compose(
        self,
        *args: str,
        filename: Path | None = None,
        envfile: Path | None = None,
        **options: Unpack[release.CommandOptions],
    ) -> bytes:
        """Resolve only the maintenance service whose creation the fixture owns."""
        _ = filename, envfile, options
        self.commands.append(args)
        if args == ("ps", "--all", "--quiet", "migrate"):
            return CONTAINER.encode() if self.present else b""
        if args == ("ps", "--all", "--quiet"):
            return b""
        message = "Unmodelled fixture Compose operation"
        raise AssertionError(message)

    def container(self, service: str) -> JsonObject:
        """Fail on incidental container discovery outside this test's expected path."""
        _ = service
        message = "Unmodelled fixture service operation"
        raise AssertionError(message)


def runtime_container() -> JsonObject:
    """Describe the exact isolated migration container expected by the guest helper."""
    return {
        "id": CONTAINER,
        "name": "/simplestchat-public-migrate-1",
        "image": IMAGE,
        "user": "10001:10001",
        "labels": {
            "com.docker.compose.project": "simplestchat-public",
            "com.docker.compose.service": "migrate",
        },
        "state": {"Running": True, "OOMKilled": False},
        "host": {
            "ReadonlyRootfs": True,
            "Privileged": False,
            "CapDrop": ["ALL"],
            "CapAdd": None,
            "SecurityOpt": ["no-new-privileges:true"],
            "Memory": 1024**3,
            "PidsLimit": 256,
            "NanoCpus": 10**9,
            "NetworkMode": "none",
        },
    }


def configured_migrate() -> JsonObject:
    """Represent the current Compose JSON model, before container creation."""
    return {
        "user": "10001:10001",
        "read_only": True,
        "cap_drop": ["ALL"],
        "security_opt": ["no-new-privileges:true"],
        "mem_limit": "1073741824",
        "pids_limit": 256,
        "cpus": 1.0,
        "network_mode": "none",
        "init": True,
    }


def unit_properties() -> dict[str, str]:
    """Represent effective properties from the canonical inactive benchmark unit."""
    return {
        "LoadState": "loaded",
        "FragmentPath": "/etc/systemd/system/simplestchat-benchmark.service",
        "User": "root",
        "DynamicUser": "no",
        "UMask": "0077",
        "RuntimeDirectory": "simplestchat-bench",
        "RuntimeDirectoryMode": "0700",
        "RuntimeDirectoryPreserve": "yes",
        "ActiveState": "inactive",
        "UnitFileState": "static",
    }


class GuestGuardTests(unittest.TestCase):
    """Guard failures occur before commands or mutable fixture state can be created."""

    def test_stage_installs_complete_imports_before_public_preparation(self) -> None:
        """A fresh guest can import staged helpers without checkout paths or prior installs."""
        tasks = objects(yaml_value((ROOT / "security/vm/stage.yml").read_text()), 0, "tasks")
        installation = next(
            task
            for task in tasks
            if task["name"] == "Install the unchanged maintained release and backup implementations"
        )
        names = strings(installation, "loop")
        self.assertLess(names.index("runtime_profile.py"), names.index("release_public.py"))
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            for name in names:
                self.assertEqual(Path(name).name, name)
                _ = (directory / name).write_bytes((ROOT / "ops/ansible/files" / name).read_bytes())
            command = [
                sys.executable,
                "-I",
                "-B",
                "-c",
                "import importlib, pathlib, sys; sys.path.insert(0, sys.argv[1]); "
                + "[importlib.import_module(name) for name in "
                + "('release_public', 'backup_public', 'restore_verify')]; "
                + "assert pathlib.Path(sys.modules['runtime_profile'].__file__) "
                + "== pathlib.Path(sys.argv[1]) / 'runtime_profile.py'",
                str(directory),
            ]
            status, _output, error = bounded_process.run(
                command,
                cwd=directory,
                env={},
                limits=bounded_process.Limits(timeout=5, stdout=65536, stderr=65536),
            )
            self.assertEqual(status, 0, error.decode())
            (directory / "runtime_profile.py").unlink()
            status, _output, error = bounded_process.run(
                command,
                cwd=directory,
                env={},
                limits=bounded_process.Limits(timeout=5, stdout=65536, stderr=65536),
            )
            self.assertNotEqual(status, 0)
            self.assertIn(b"ModuleNotFoundError: No module named 'runtime_profile'", error)

    def test_runtime_profile_permissions_are_checked_before_the_daemon(self) -> None:
        """The newly required installed dependency receives the same origin and mode checks."""
        library = ROOT / "ops/ansible/files"

        def read(path: Path, *, mode: int = 0o600) -> bytes:
            if path == guest.MARKER:
                return (RUN_ID + "\n").encode()
            if path == guest.ROOT / "selection.json":
                return json.dumps(
                    {"schemaVersion": 1, "runId": RUN_ID, "revision": REVISION}
                ).encode()
            if path == library / "runtime_profile.py":
                self.assertEqual(mode, 0o644)
                reason = "fixture_runtime_profile_permissions"
                raise release.ReleaseError(reason)
            return b"fixture helper"

        with (
            patch.object(os, "geteuid", return_value=0),
            patch.object(platform, "system", return_value="Linux"),
            patch.object(platform, "machine", return_value="x86_64"),
            patch.object(
                platform,
                "freedesktop_os_release",
                return_value={"ID": "debian", "VERSION_ID": "13"},
            ),
            patch.object(Path, "open", return_value=io.BytesIO(b"QEMU\n")),
            patch.object(Path, "lstat") as inspect_path,
            patch.object(guest, "directory"),
            patch.object(guest, "LIBRARY", library),
            patch.object(guest, "private_file", side_effect=read),
            self.assertRaisesRegex(release.ReleaseError, "fixture_runtime_profile_permissions"),
        ):
            _ = guest.guard(RUN_ID)
        inspect_path.assert_not_called()

    def test_selection_has_one_strict_schema_and_owner(self) -> None:
        """Booleans, duplicate fields, extra options and foreign runs cannot select a guest."""
        valid = {"schemaVersion": 1, "runId": RUN_ID, "revision": REVISION}
        self.assertEqual(
            guest.selection(json.dumps(valid).encode(), RUN_ID),
            guest.Selection(run_id=RUN_ID, revision=REVISION),
        )
        for replacement in (
            {"schemaVersion": True},
            {"runId": "c" * 32},
            {"revision": "B" * 40},
            {"host": "example.test"},
        ):
            with self.subTest(replacement=replacement), self.assertRaises(release.ReleaseError):
                _ = guest.selection(json.dumps({**valid, **replacement}).encode(), RUN_ID)
        with self.assertRaises(ValueError):
            _ = guest.selection(b'{"schemaVersion":1,"schemaVersion":1}', RUN_ID)

    def test_nonroot_refused_before_environment_or_commands(self) -> None:
        """An accidental ordinary-host invocation cannot reach even the VM identity check."""
        with (
            patch.object(os, "geteuid", return_value=1000),
            patch.object(platform, "system") as system,
        ):
            with self.assertRaisesRegex(release.ReleaseError, "requires root"):
                _ = guest.guard(RUN_ID)
            system.assert_not_called()

    def test_platform_and_debian_version_are_required(self) -> None:
        """Root privileges alone do not authorize a different host or distribution."""
        with (
            patch.object(os, "geteuid", return_value=0),
            patch.object(platform, "system", return_value="Darwin"),
            patch.object(platform, "machine", return_value="x86_64"),
            self.assertRaisesRegex(release.ReleaseError, "Linux x86_64"),
        ):
            _ = guest.guard(RUN_ID)
        with (
            patch.object(os, "geteuid", return_value=0),
            patch.object(platform, "system", return_value="Linux"),
            patch.object(platform, "machine", return_value="x86_64"),
            patch.object(
                platform,
                "freedesktop_os_release",
                return_value={"ID": "debian", "VERSION_ID": "12"},
            ),
            self.assertRaisesRegex(release.ReleaseError, "Debian 13"),
        ):
            _ = guest.guard(RUN_ID)

    def test_qemu_marker_is_required_before_private_state(self) -> None:
        """A Debian host with an unrelated DMI identity cannot reach fixture files."""
        with (
            patch.object(os, "geteuid", return_value=0),
            patch.object(platform, "system", return_value="Linux"),
            patch.object(platform, "machine", return_value="x86_64"),
            patch.object(
                platform,
                "freedesktop_os_release",
                return_value={"ID": "debian", "VERSION_ID": "13"},
            ),
            patch.object(Path, "open", return_value=io.BytesIO(b"Other vendor\n")),
            patch.object(guest, "private_file") as private,
        ):
            with self.assertRaisesRegex(release.ReleaseError, "QEMU"):
                _ = guest.guard(RUN_ID)
            private.assert_not_called()

    def test_private_file_rejects_links_modes_and_unbounded_inputs(self) -> None:
        """Exercise actual file descriptors and modes using this test process's own files."""
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "selection.json"
            _ = path.write_bytes(b"fixed-fixture-data")
            path.chmod(0o600)
            options = {"uid": os.getuid(), "gid": os.getgid()}
            self.assertEqual(guest.private_file(path, **options), b"fixed-fixture-data")
            path.chmod(0o644)
            with self.assertRaises(release.ReleaseError):
                _ = guest.private_file(path, **options)
            path.chmod(0o600)
            alias = path.with_name("alias")
            alias.symlink_to(path)
            with self.assertRaises(OSError):
                _ = guest.private_file(alias, **options)
            alias.unlink()
            alias.hardlink_to(path)
            with self.assertRaises(release.ReleaseError):
                _ = guest.private_file(path, **options)
            alias.unlink()
            _ = path.write_bytes(b"x" * (guest.MAX_FILE + 1))
            with self.assertRaises(release.ReleaseError):
                _ = guest.private_file(path, **options)

    def test_private_marker_must_match_the_requested_run(self) -> None:
        """Even the correct VM platform cannot authorize a different private ownership marker."""
        with (
            patch.object(os, "geteuid", return_value=0),
            patch.object(platform, "system", return_value="Linux"),
            patch.object(platform, "machine", return_value="x86_64"),
            patch.object(
                platform,
                "freedesktop_os_release",
                return_value={"ID": "debian", "VERSION_ID": "13"},
            ),
            patch.object(Path, "open", return_value=io.BytesIO(b"QEMU\n")),
            patch.object(guest, "directory"),
            patch.object(guest, "private_file", return_value=("c" * 32 + "\n").encode()) as private,
            self.assertRaisesRegex(release.ReleaseError, "marker differs"),
        ):
            _ = guest.guard(RUN_ID)
        private.assert_called_once_with(guest.MARKER)

    def test_phase_deadline_caps_each_command_and_stops_expired_work(self) -> None:
        """Polling shares one budget instead of allocating another timeout per command."""
        runner = guest.GuestRunner(Path("/unused-evidence"))
        runner.deadline = 12
        with (
            patch.object(time, "monotonic", return_value=10),
            patch.object(release.Runner, "run", return_value=b"") as run,
        ):
            _ = runner.run(["/usr/bin/true"], timeout=30)
            run.assert_called_once_with(["/usr/bin/true"], timeout=2)
        with (
            patch.object(time, "monotonic", return_value=12),
            patch.object(release.Runner, "run") as run,
        ):
            with self.assertRaisesRegex(release.ReleaseError, "deadline exceeded"):
                _ = runner.run(["/usr/bin/true"])
            run.assert_not_called()


class GuestAssertionTests(unittest.TestCase):
    """Check rejected service identities and sandbox drift without touching a daemon."""

    def test_snapshot_detects_changed_config_hashes_without_saving_secrets(self) -> None:
        """Two actual snapshot writes compare only protected hashes and metadata."""
        chosen = guest.Selection(run_id=RUN_ID, revision=REVISION)
        runner = RecordedRunner()
        read_private = guest.private_file

        def systemd(
            _runner: release.RunnerProtocol, unit: str, _names: tuple[str, ...]
        ) -> dict[str, str]:
            value = unit_properties()
            value["FragmentPath"] = "/etc/systemd/system/" + unit
            if unit == "simplestchat-image-build.service":
                value["RuntimeDirectory"] = ""
            return value

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)

            def read(path: Path, *, mode: int = 0o600) -> bytes:
                if path.parent == root:
                    return read_private(path, uid=os.getuid(), gid=os.getgid(), mode=mode)
                return b"fixed-unit-fixture"

            with (
                patch.object(guest, "ROOT", root),
                patch.object(
                    guest,
                    "public_files",
                    return_value={"app.env": {"sha256": "e" * 64, "mode": 0o600}},
                ) as files,
                patch.object(guest, "properties", side_effect=systemd),
                patch.object(
                    guest, "configuration", return_value={"sha256": "f" * 64, "services": 4}
                ),
                patch.object(guest, "private_file", side_effect=read),
            ):
                self.assertFalse(guest.snapshot(runner, chosen)["idempotent"])
                baseline = (root / "snapshot.json").read_bytes()
                self.assertNotIn(b"fixed-unit-fixture", baseline)
                self.assertTrue(guest.snapshot(runner, chosen)["idempotent"])
                self.assertEqual(
                    object_value(decode_json((root / "snapshot.json").read_bytes()))["passes"],
                    guest.SNAPSHOT_PASSES,
                )
                _ = (root / "snapshot.json").write_bytes(baseline)
                files.return_value = {"app.env": {"sha256": "c" * 64, "mode": 0o600}}
                with self.assertRaisesRegex(
                    release.ReleaseError, "changed protected configuration"
                ):
                    _ = guest.snapshot(runner, chosen)

    def test_workload_unit_cannot_be_enabled_active_or_foreign(self) -> None:
        """Systemd evidence must identify the expected inactive, root-owned service."""
        valid = unit_properties()
        guest.validate_unit(valid, "simplestchat-benchmark.service")
        for key, value in (
            ("User", "nobody"),
            ("ActiveState", "active"),
            ("UnitFileState", "enabled"),
            ("FragmentPath", "/run/foreign.service"),
            ("UMask", "0022"),
            ("RuntimeDirectoryMode", "0755"),
            ("RuntimeDirectoryPreserve", "no"),
        ):
            with self.subTest(key=key), self.assertRaises(release.ReleaseError):
                guest.validate_unit({**valid, key: value}, "simplestchat-benchmark.service")

    def test_systemd_properties_cannot_be_missing_or_duplicated(self) -> None:
        """A successful command with an incomplete report never proves unit configuration."""
        runner = RecordedRunner()
        runner.output = b"User=root\nUMask=0077\n"
        self.assertEqual(
            guest.properties(runner, "fixture.service", ("User", "UMask")),
            {"User": "root", "UMask": "0077"},
        )
        for output in (
            b"User=root\n",
            b"User=root\nUser=nobody\nUMask=0077\n",
            b"User=root\nUnknown=yes\nUMask=0077\n",
        ):
            runner.output = output
            with self.subTest(output=output), self.assertRaises(release.ReleaseError):
                _ = guest.properties(runner, "fixture.service", ("User", "UMask"))

    def test_configured_sandbox_requires_all_controls(self) -> None:
        """Dropping a single declared capability or resource guard must fail the fixture."""
        valid = configured_migrate()
        guest.configured_service(valid, "migrate")
        changes: list[JsonObject] = [
            {"user": "0:0"},
            {"read_only": False},
            {"cap_drop": []},
            {"cap_add": ["SYS_ADMIN"]},
            {"security_opt": []},
            {"privileged": True},
            {"mem_limit": "0"},
            {"pids_limit": -1},
            {"cpus": 0},
            {"network_mode": "host"},
        ]
        for replacement in changes:
            with self.subTest(replacement=replacement), self.assertRaises(release.ReleaseError):
                guest.configured_service({**valid, **replacement}, "migrate")
        app: JsonObject = {
            **valid,
            "ports": [
                {"protocol": "tcp", "target": 3000, "published": "3000", "host_ip": "127.0.0.1"}
            ],
        }
        guest.configured_service(app, "simplestchat")
        app["ports"] = [{"protocol": "tcp", "target": 3000, "published": "3000", "host_ip": "::"}]
        with self.assertRaisesRegex(release.ReleaseError, "loopback-only"):
            guest.configured_service(app, "simplestchat")

    def test_configured_memory_requires_bounded_canonical_decimal_bytes(self) -> None:
        """Reject coercions and unbounded, noncanonical or out-of-range memory declarations."""
        valid = configured_migrate()
        for memory in ("1", str(guest.MAX_MEMORY)):
            with self.subTest(memory=memory):
                guest.configured_service({**valid, "mem_limit": memory}, "migrate")
        invalid: list[JsonValue] = [
            None,
            True,
            1073741824,
            1073741824.0,
            [],
            {},
            "",
            "0",
            "-1",
            "+1",
            "01",
            " 1",
            "1 ",
            "1\n",
            "1.0",
            "1e9",
            "1g",
            "\u0661",
            str(guest.MAX_MEMORY + 1),
            "9" * 10000,
        ]
        for memory in invalid:
            with (
                self.subTest(memory=memory),
                self.assertRaisesRegex(release.ReleaseError, "resource limits differ"),
            ):
                guest.configured_service({**valid, "mem_limit": memory}, "migrate")

    def test_running_service_checks_actual_identity_and_limits(self) -> None:
        """A correct Compose declaration cannot substitute for real runtime metadata."""
        value = runtime_container()
        self.assertEqual(guest.running_service(value, "migrate", IMAGE), CONTAINER)
        for section, key, replacement in (
            ("host", "Memory", 0),
            ("host", "Memory", "1073741824"),
            ("host", "NanoCpus", 0),
            ("host", "ReadonlyRootfs", False),
            ("host", "NetworkMode", "host"),
            ("state", "OOMKilled", True),
        ):
            changed = deepcopy(value)
            object_value(changed[section])[key] = replacement
            with self.subTest(section=section, key=key), self.assertRaises(release.ReleaseError):
                _ = guest.running_service(changed, "migrate", IMAGE)
        with self.assertRaises(release.ReleaseError):
            _ = guest.running_service({**value, "image": "sha256:" + "e" * 64}, "migrate", IMAGE)

    def test_cleanup_refuses_image_name_label_or_id_mismatch(self) -> None:
        """No stop/remove command is issued after the maintenance identity changes."""
        changes: list[JsonObject] = [
            {"image": "sha256:" + "e" * 64},
            {"name": "/other-migrate-1"},
            {"id": "f" * 64},
            {
                "labels": {
                    "com.docker.compose.project": "other",
                    "com.docker.compose.service": "migrate",
                }
            },
        ]
        for replacement in changes:
            runner = RecordedRunner()
            runner.metadata.update(replacement)
            with self.subTest(replacement=replacement), self.assertRaises(release.ReleaseError):
                guest.remove_migrate(runner, IMAGE, CONTAINER)
            self.assertFalse(any(command[0] in ("stop", "rm") for command in runner.commands))

    def test_cleanup_uses_one_exact_id_and_confirms_disappearance(self) -> None:
        """The command model permits only ID-scoped stop/remove and a final service lookup."""
        runner = RecordedRunner()
        guest.remove_migrate(runner, IMAGE, CONTAINER)
        self.assertIn(("stop", "--time", "10", CONTAINER), runner.commands)
        self.assertIn(("rm", CONTAINER), runner.commands)
        self.assertFalse(runner.present)
        self.assertEqual(runner.commands[-1], ("ps", "--all", "--quiet", "migrate"))


class ComposeSerializationTests(unittest.TestCase):
    """Exercise the pinned real renderer without an engine or application processes."""

    def renderer(self, directory: Path, environment: dict[str, str]) -> list[str]:
        """Find exactly the provisioned Compose version; the deployment CI requires it."""
        defaults = object_value(
            yaml_value((ROOT / "ops/ansible/group_vars/benchmark_hosts.yml").read_text())
        )
        expected = string_value(defaults["scbench_docker_compose_version"]).partition("-")[0]
        candidates: list[list[str]] = []
        standalone, docker = shutil.which("docker-compose"), shutil.which("docker")
        if standalone is not None:
            candidates.append([standalone])
        if docker is not None:
            candidates.append([docker, "compose"])
        for command in candidates:
            status, output, _error = bounded_process.run(
                [*command, "version", "--short"],
                cwd=directory,
                env=environment,
                limits=bounded_process.Limits(timeout=10, stdout=1024, stderr=4096),
            )
            if status == 0 and output.strip() == expected.encode():
                return command
        if os.environ.get("VM_COMPOSE_REQUIRED") == "1":
            self.fail("The provisioned Compose version is required for this regression")
        reason = "Optional inert renderer check requires the provisioned Compose version"
        raise unittest.SkipTest(reason)

    def test_real_pinned_compose_model_passes_guest_checks(self) -> None:
        """The actual four-service JSON model satisfies the independent guest assertions."""
        with tempfile.TemporaryDirectory(prefix="simplestchat-vm-compose.") as temporary:
            directory = Path(temporary).resolve()
            environment = {
                "PATH": "/usr/bin:/bin",
                "HOME": str(directory),
                "LC_ALL": "C",
                # No engine exists at this private path; config must remain daemon-free.
                "DOCKER_HOST": "unix://" + str(directory / "absent.sock"),
            }
            command = self.renderer(directory, environment)
            for template, name in (
                ("public-compose.yml.j2", "compose.public.yml"),
                ("public-app.env.j2", "app.env"),
                ("public-migration.env.j2", "migration.env"),
                ("public-proxy.env.j2", "proxy.env"),
            ):
                _ = (directory / name).write_text(
                    templates.render(
                        template,
                        scpub_config=str(directory),
                        scpub_root=str(directory / "data"),
                        scpub_domain="vm-fixture.test",
                        scpub_announce_ip="127.0.0.1",
                        scpub_announce_ipv6="",
                        scpub_media_workers=1,
                        scpub_app_cpus=1,
                        scpub_app_memory_mib=1024,
                        scpub_postgres_memory_mib=1024,
                        scpub_postgres_shared_buffers_mib=256,
                    )
                    + "\n"
                )
            _ = (directory / "compose.base.yml").write_bytes(
                (ROOT / "docker-compose.yml").read_bytes()
            )
            status, output, error = bounded_process.run(
                [
                    *command,
                    "--env-file",
                    str(directory / "app.env"),
                    "-f",
                    str(directory / "compose.public.yml"),
                    "--profile",
                    "maintenance",
                    "config",
                    "--format",
                    "json",
                ],
                cwd=directory,
                env=environment,
                limits=bounded_process.Limits(timeout=20, stdout=65536, stderr=4096),
            )
            self.assertEqual(status, 0, error.decode())
            runner = RecordedRunner()
            runner.output = output
            self.assertEqual(guest.configuration(runner)["services"], 4)
            services = object_value(object_value(decode_json(output))["services"])
            for name in ("simplestchat", "migrate", "postgres"):
                self.assertEqual(object_value(services[name])["mem_limit"], "1073741824")


if __name__ == "__main__":
    _ = unittest.main()
