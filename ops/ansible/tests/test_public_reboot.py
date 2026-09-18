"""Offline reboot lifecycle tests; no host, Docker daemon or reboot is contacted.

The fixture accepts only the small Docker/Compose command set permitted during
an existing-container reboot. Journal, identity, readiness and recovery decisions
run against private temporary files; ownership and Linux boot IDs are mocked.
"""

import io
import json
import os
import signal
import stat
import sys
import tempfile
import time
import unittest
from collections.abc import Sequence
from contextlib import redirect_stdout
from copy import deepcopy
from pathlib import Path
from typing import Protocol, Unpack, final, override
from unittest.mock import patch

# Fixture assertions and literal expected limits document the tested contract.
# ruff: noqa: S101, PLR2004
import test_support

# isort: split
import reboot_public as reboot
import release_public as public
from release_artifact import sha256_file
from release_json import JsonObject, decode_json
from test_support import at, obj, objects, string, strings, yaml_value

ROOT = test_support.ROOT
FILES = ROOT / "ops/ansible/files"


def json_object(data: str | bytes) -> JsonObject:
    """Decode fixture evidence with the same checked JSON boundary as production."""
    return obj(decode_json(data))


OLD_BOOT = "11111111-1111-1111-1111-111111111111"
NEW_BOOT = "22222222-2222-2222-2222-222222222222"
REVISION = "a" * 40
APP_IMAGE = "sha256:" + "a" * 64
PG_IMAGE = "sha256:" + "b" * 64
PROXY_IMAGE = "sha256:" + "c" * 64
PG_SELECTOR = "docker.io/library/postgres:fixture@sha256:" + "d" * 64
PROXY_SELECTOR = "docker.io/library/caddy:fixture@sha256:" + "e" * 64


class PatchHandle(Protocol):
    """Expose reversible patch lifecycle without dynamic mock return values."""

    def start(self) -> object:
        """Activate the patch and return its opaque replacement."""
        ...

    def stop(self) -> object:
        """Restore the original attribute."""
        ...


@final
class FixtureRunner:
    """Reject unapproved commands rather than supplying a permissive mock."""

    def __init__(self, state: "PublicRebootTests", attempt: Path) -> None:
        """Retain the explicit fixture state and private attempt directory."""
        self.state = state
        self.attempt = attempt

    def compose(  # noqa: C901, PLR0911 - explicitly allow each reboot command; unexpected operations fail.
        self,
        *args: str,
        filename: Path | None = None,
        envfile: Path | None = None,
        **kwargs: Unpack[public.CommandOptions],
    ) -> bytes:
        """Model only the Compose operations allowed during reboot recovery."""
        assert filename is None
        assert envfile is None
        self.state.calls.append(("compose", args, dict(kwargs)))
        if args == ("start", "--help"):
            return self.state.start_help.encode()
        if len(args) == 4 and args[:3] == ("ps", "--all", "--quiet"):
            service = args[-1]
            value = self.state.containers[service]
            return (string(value, "id") if value else "").encode()
        if args[:2] == ("config", "--hash"):
            service = args[-1]
            value = string(self.state.containers[service], "configHash")
            if service == self.state.changed_hash:
                value = "f" * 64
            return f"{service} {value}\n".encode()
        if args == ("config", "--format", "json"):
            return json.dumps(self.state.resolved).encode()
        if args == ("--profile", "maintenance", "ps", "--all", "--quiet", "migrate"):
            return b"f" * 64 if self.state.migration_container else b""
        if args[0] == "stop":
            service = args[-1]
            assert args == ("stop", "--timeout", "60" if service == "postgres" else "30", service)
            value = self.state.containers[service]
            obj(value, "state").update(Running=False, ExitCode=0)
            if service == self.state.stop_failure:
                self.state.stop_failure = None
                message = "fixture graceful stop failed"
                raise public.ReleaseError(message)
            if service == self.state.unclean_stop:
                obj(value, "state")["ExitCode"] = 137
            return b""
        if args[0] == "start":
            service = args[-1]
            assert args == (
                ("start", "--wait", "--wait-timeout", "180", "postgres")
                if service == "postgres"
                else ("start", service)
            )
            value = self.state.containers[service]
            obj(value, "state")["Running"] = True
            if service == "postgres":
                obj(value, "state", "Health")["Status"] = (
                    "unhealthy" if self.state.unhealthy_resume else "healthy"
                )
            return b""
        message = f"Unexpected Compose command: {args}"
        raise AssertionError(message)

    def docker(self, *args: str, **kwargs: Unpack[public.CommandOptions]) -> bytes:
        """Inspect the fixed selected images and the retained fixture containers."""
        self.state.calls.append(("docker", args, dict(kwargs)))
        if args[:2] == ("image", "inspect"):
            selector = args[-1]
            if selector in (PG_SELECTOR, PROXY_SELECTOR):
                return (PG_IMAGE if selector == PG_SELECTOR else PROXY_IMAGE).encode()
            assert selector == APP_IMAGE
            return json.dumps(
                {
                    "id": APP_IMAGE,
                    "os": "linux",
                    "architecture": "amd64",
                    "user": "10001:10001",
                    "labels": {"org.opencontainers.image.revision": REVISION},
                    "cmd": ["/app/simplestChat"],
                    "entrypoint": None,
                }
            ).encode()
        if args[0] == "inspect":
            return json.dumps(
                next(
                    value
                    for value in self.state.containers.values()
                    if value and value["id"] == args[-1]
                )
            ).encode()
        message = f"Unexpected Docker command: {args}"
        raise AssertionError(message)

    def run(self, args: Sequence[str], **kwargs: Unpack[public.CommandOptions]) -> bytes:
        """Model readiness only after all required existing services are running."""
        self.state.calls.append(("run", tuple(args), dict(kwargs)))
        assert args[0] == "/usr/bin/curl"
        assert args[-1].endswith("/ready")
        assert "--insecure" not in args
        assert "--location" not in args
        assert at(self.state.containers["postgres"], "state", "Running")
        assert at(self.state.containers["simplestchat"], "state", "Running")
        if args[-1].startswith("https:"):
            assert at(self.state.containers["caddy"], "state", "Running")
        if self.state.unready or (self.state.proxy_unready and args[-1].startswith("https:")):
            message = "fixture readiness unavailable"
            raise public.ReleaseError(message)
        return b'{"status":"ready"}'

    def container(self, service: str) -> JsonObject:
        """Expose the checked inspection required by the public runner contract."""
        return reboot.container(self, service)


@final
class PublicRebootTests(unittest.TestCase):
    """Prove reboot preparation, recovery, and failure ownership without host access."""

    def __init__(self, methodName: str = "runTest") -> None:  # noqa: N803 - unittest's public constructor keyword.
        """Initialize harmless typed defaults; setUp owns all resource creation."""
        super().__init__(methodName)
        self.temporary: tempfile.TemporaryDirectory[str] | None = None
        self.root = Path()
        self.config = Path()
        self.work = Path()
        self.before: dict[str, bytes] = {}
        self.current_boot = OLD_BOOT
        self.calls: list[tuple[str, tuple[str, ...], dict[str, object]]] = []
        self.containers: dict[str, JsonObject | None] = {}
        self.resolved: JsonObject = {}
        self.changed_hash: str | None = None
        self.migration_container = False
        self.stop_failure: str | None = None
        self.unclean_stop: str | None = None
        self.unhealthy_resume = False
        self.unready = False
        self.proxy_unready = False
        self.start_help = ""
        self.protection_calls: list[Path] = []
        self.patches: list[PatchHandle] = []
        self.previous_umask = 0

    @override
    def setUp(self) -> None:
        """Create isolated fixtures and register reversible test substitutions."""
        self.temporary = tempfile.TemporaryDirectory(prefix="simplestchat-public-reboot-test.")
        self.root = Path(self.temporary.name).resolve()
        self.config = self.root / "config"
        self.work = self.root / "work"
        for directory in (self.config, self.work, self.root / "results"):
            directory.mkdir(mode=0o700)
        (self.work / "workload.lock").touch(mode=0o600)
        for name, mode in reboot.CONFIGURATION.items():
            _ = (self.config / name).write_text(f"private fixture {name}\n")
            (self.config / name).chmod(mode)
        selected = {
            "revision": REVISION,
            "serverImage": APP_IMAGE,
            "postgresImage": PG_SELECTOR,
            "caddyImage": PROXY_SELECTOR,
        }
        _ = (self.config / "images.json").write_text(json.dumps(selected))
        self.before = {name: (self.config / name).read_bytes() for name in reboot.CONFIGURATION}
        self.current_boot = OLD_BOOT
        self.calls = []
        self.containers = {}
        for index, (service, image) in enumerate(
            (("simplestchat", APP_IMAGE), ("postgres", PG_IMAGE), ("caddy", PROXY_IMAGE)), 1
        ):
            self.containers[service] = {
                "id": str(index) * 64,
                "image": image,
                "configHash": str(index) * 64,
                "state": {"Running": True, "OOMKilled": False, "ExitCode": 0},
            }
        obj(self.containers["postgres"], "state")["Health"] = {"Status": "healthy"}
        self.resolved = {
            "services": {service: {"restart": "unless-stopped"} for service in reboot.SERVICES}
        }
        obj(self.resolved, "services", "simplestchat")["environment"] = {
            "RUN_MIGRATIONS": "false",
            "WEBAUTHN_ORIGIN": "https://fixture.invalid",
        }
        self.changed_hash = None
        self.migration_container = False
        self.stop_failure = None
        self.unclean_stop = None
        self.unhealthy_resume = False
        self.unready = False
        self.proxy_unready = False
        self.start_help = (
            "      --wait             Wait for services\n      --wait-timeout int  Maximum wait\n"
        )
        self.protection_calls = []
        self.patches = [
            patch.object(public, "ROOT", self.root),
            patch.object(public, "CONFIG", self.config),
            patch.object(public, "WORK", self.work),
            patch.object(public, "protected", side_effect=self.protected),
            patch.object(public, "Runner", side_effect=self.runner),
            patch.object(public, "boot_id", side_effect=lambda: self.current_boot),
            patch.object(os, "geteuid", return_value=0),
            patch.object(signal, "signal"),
            patch.object(time, "monotonic", side_effect=iter(range(0, 100000, 100))),
            patch.object(time, "sleep"),
        ]
        for value in self.patches:
            _ = value.start()
        self.previous_umask = os.umask(0o077)

    @override
    def tearDown(self) -> None:
        """Restore process state and remove only owned temporary fixtures."""
        _ = os.umask(self.previous_umask)
        for value in reversed(self.patches):
            _ = value.stop()
        if self.temporary is not None:
            self.temporary.cleanup()

    def runner(self, attempt: Path) -> FixtureRunner:
        """Create one typed command fixture for the requested evidence owner."""
        return FixtureRunner(self, attempt)

    def protected(
        self,
        path: Path,
        *,
        directory: bool = False,
        modes: tuple[int, ...] = (0o600,),
        limit: int | None = None,
    ) -> None:
        """Exercise real fixture type/mode/size checks, replacing only root ownership."""
        self.protection_calls.append(path)
        metadata = path.lstat()
        public.require(stat.S_IMODE(metadata.st_mode) in modes, "Fixture permission mismatch")
        public.require(
            stat.S_ISDIR(metadata.st_mode) if directory else stat.S_ISREG(metadata.st_mode),
            "Fixture type mismatch",
        )
        public.require(limit is None or metadata.st_size <= limit, "Fixture size exceeded")

    def execute(self, action: str) -> JsonObject:
        """Run the real CLI transaction with captured public output."""
        with (
            patch.object(sys, "argv", ["reboot-public.py", action]),
            redirect_stdout(io.StringIO()) as output,
        ):
            reboot.main()
        return json_object(output.getvalue())

    def state(self) -> JsonObject:
        """Read the durable operation journal through the checked JSON boundary."""
        return json_object((self.root / "release-state.json").read_text())

    def attempt(self) -> Path:
        """Select the only expected preparation evidence directory."""
        values = list((self.root / "results").glob("reboot.*"))
        self.assertEqual(len(values), 1)
        return values[0]

    def report(self) -> JsonObject:
        """Read the original preparation's current outcome evidence."""
        return json_object((self.attempt() / "outcome.json").read_text())

    def mutations(self) -> list[tuple[str, ...]]:
        """List only the service start and stop calls that can affect availability."""
        return [
            args
            for kind, args, _ in self.calls
            if kind == "compose" and args[0] in ("stop", "start") and "--help" not in args
        ]

    def test_older_compose_is_rejected_before_any_stops_or_unfinished_journal(self) -> None:
        """Older compose is rejected before any stops or unfinished journal."""
        for help_text in ("Usage: compose start\n", "      --wait-timeout int  Maximum wait\n"):
            with self.subTest(help_text=help_text):
                self.start_help = help_text
                with self.assertRaisesRegex(public.ReleaseError, "Installed Compose must support"):
                    _ = self.execute("prepare")
                self.assertEqual(self.mutations(), [])
                self.assertFalse((self.root / "release-state.json").exists())

    def test_prepare_stops_only_existing_public_services_in_order_and_retains_unfinished_evidence(
        self,
    ) -> None:
        """Prepare stops only existing public services in order and retains unfinished evidence."""
        output = self.execute("prepare")
        self.assertEqual(
            self.mutations(),
            [
                ("stop", "--timeout", "30", "simplestchat"),
                ("stop", "--timeout", "30", "caddy"),
                ("stop", "--timeout", "60", "postgres"),
            ],
        )
        state = self.state()
        self.assertEqual(state["bootId"], OLD_BOOT)
        self.assertEqual(state["phase"], "await_reboot")
        self.assertIs(state["finalized"], expr2=False)
        self.assertIs(
            output["passed"], expr2=False, msg="Preparation must not claim the reboot completed"
        )
        self.assertEqual(state["identitySha256"], sha256_file(self.attempt() / "identity.json"))
        self.assertEqual(
            {name: (self.config / name).read_bytes() for name in reboot.CONFIGURATION}, self.before
        )
        for path in (
            self.attempt() / "identity.json",
            self.attempt() / "outcome.json",
            self.root / "release-state.json",
        ):
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)

    def test_changed_boot_resumes_same_containers_in_dependency_order_and_finalizes(self) -> None:
        """Changed boot resumes same containers in dependency order and finalizes."""
        _ = self.execute("prepare")
        self.current_boot = NEW_BOOT
        self.calls.clear()
        output = self.execute("resume")
        self.assertEqual(
            self.mutations(),
            [
                ("start", "--wait", "--wait-timeout", "180", "postgres"),
                ("start", "simplestchat"),
                ("start", "caddy"),
            ],
        )
        self.assertIs(output["passed"], expr2=True)
        self.assertIs(self.state()["finalized"], expr2=True)
        self.assertEqual(self.state()["phase"], "reboot_complete")
        self.assertEqual(self.report()["recoveryBootId"], NEW_BOOT)
        self.assertEqual(Path(string(self.state(), "attempt")), self.attempt())
        self.assertTrue((self.attempt() / "resume").is_dir())

    def test_same_boot_resume_refuses_without_starting_anything(self) -> None:
        """Same boot resume refuses without starting anything."""
        _ = self.execute("prepare")
        self.calls.clear()
        with self.assertRaises(public.ReleaseError):
            _ = self.execute("resume")
        self.assertEqual(self.calls, [])
        self.assertEqual(self.state()["phase"], "await_reboot")

    def test_cancel_restores_original_boot_but_preserves_failed_reboot_outcome(self) -> None:
        """Cancel restores original boot but preserves failed reboot outcome."""
        _ = self.execute("prepare")
        self.calls.clear()
        output = self.execute("cancel")
        self.assertEqual(
            [args[-1] for args in self.mutations()], ["postgres", "simplestchat", "caddy"]
        )
        self.assertIs(output["passed"], expr2=False)
        self.assertIs(self.report()["recoveryPassed"], expr2=True)
        self.assertIn("did not complete", string(self.report(), "failure"))
        self.assertIs(self.state()["finalized"], expr2=True)
        self.assertEqual(self.state()["phase"], "reboot_cancelled")

    def test_cancel_refuses_a_changed_boot(self) -> None:
        """Cancel refuses a changed boot."""
        _ = self.execute("prepare")
        self.current_boot = NEW_BOOT
        self.calls.clear()
        with self.assertRaises(public.ReleaseError):
            _ = self.execute("cancel")
        self.assertEqual(self.calls, [])

    def test_preparation_failure_recovers_once_and_preserves_original_failure(self) -> None:
        """Preparation failure recovers once and preserves original failure."""
        self.stop_failure = "caddy"
        with self.assertRaisesRegex(public.ReleaseError, "fixture graceful stop failed"):
            _ = self.execute("prepare")
        report = self.report()
        self.assertIs(report["passed"], expr2=False)
        self.assertIs(report["recoveryPassed"], expr2=True)
        self.assertEqual(report["failure"], "fixture graceful stop failed")
        self.assertEqual(
            [args[-1] for args in self.mutations() if args[0] == "start"],
            ["postgres", "simplestchat", "caddy"],
        )
        self.assertIs(self.state()["finalized"], expr2=True)
        self.assertEqual(self.state()["phase"], "reboot_prepare_failed_recovered")

    def test_failed_preparation_recovery_keeps_unfinished_evidence_and_original_failure(
        self,
    ) -> None:
        """Failed preparation recovery keeps unfinished evidence and original failure."""
        self.stop_failure = "caddy"
        self.unhealthy_resume = True
        with self.assertRaisesRegex(public.ReleaseError, "fixture graceful stop failed"):
            _ = self.execute("prepare")
        self.assertIs(self.report()["recoveryPassed"], expr2=False)
        self.assertIs(self.state()["finalized"], expr2=False)
        self.assertEqual(self.state()["phase"], "stop_for_reboot")
        self.assertEqual(
            [args[-1] for args in self.mutations() if args[0] == "start"], ["postgres"]
        )

    def test_force_killed_container_does_not_count_as_graceful_preparation(self) -> None:
        """Force killed container does not count as graceful preparation."""
        self.unclean_stop = "simplestchat"
        with self.assertRaisesRegex(public.ReleaseError, "did not stop cleanly"):
            _ = self.execute("prepare")
        self.assertIs(self.report()["passed"], expr2=False)
        self.assertIs(self.report()["recoveryPassed"], expr2=True)

    def test_unhealthy_database_rejects_preparation_before_stopping_services(self) -> None:
        """Unhealthy database rejects preparation before stopping services."""
        obj(self.containers["postgres"], "state", "Health")["Status"] = "unhealthy"
        with self.assertRaisesRegex(public.ReleaseError, "Database must be healthy"):
            _ = self.execute("prepare")
        self.assertEqual(self.mutations(), [])
        self.assertFalse((self.root / "release-state.json").exists())

    def test_runtime_migrations_or_other_restart_policy_reject_preparation(self) -> None:
        """Runtime migrations or other restart policy reject preparation."""
        for field, value in (("RUN_MIGRATIONS", "true"), ("restart", "always")):
            with self.subTest(field=field):
                original = deepcopy(self.resolved)
                if field == "restart":
                    obj(self.resolved, "services", "simplestchat")[field] = value
                else:
                    obj(self.resolved, "services", "simplestchat", "environment")[field] = value
                with self.assertRaises(public.ReleaseError):
                    _ = self.execute("prepare")
                self.assertEqual(self.mutations(), [])
                self.resolved = original

    def test_changed_running_configuration_or_retained_migration_rejects_preparation(self) -> None:
        """Changed running configuration or retained migration rejects preparation."""
        self.changed_hash = "caddy"
        with self.assertRaisesRegex(public.ReleaseError, "configuration differs"):
            _ = self.execute("prepare")
        self.assertEqual(self.mutations(), [])
        self.changed_hash = None
        self.migration_container = True
        with self.assertRaisesRegex(public.ReleaseError, "migration container"):
            _ = self.execute("prepare")
        self.assertEqual(self.mutations(), [])

    def test_changed_configuration_after_reboot_refuses_service_starts(self) -> None:
        """Changed configuration after reboot refuses service starts."""
        _ = self.execute("prepare")
        self.current_boot = NEW_BOOT
        _ = (self.config / "Caddyfile").write_text("changed proxy config\n")
        self.calls.clear()
        with self.assertRaisesRegex(public.ReleaseError, "Configuration changed"):
            _ = self.execute("resume")
        self.assertEqual(self.mutations(), [])
        self.assertIs(self.state()["finalized"], expr2=False)

    def test_changed_container_after_reboot_is_not_recreated(self) -> None:
        """Changed container after reboot is not recreated."""
        _ = self.execute("prepare")
        self.current_boot = NEW_BOOT
        obj(self.containers["simplestchat"])["id"] = "f" * 64
        self.calls.clear()
        with self.assertRaisesRegex(public.ReleaseError, "container changed"):
            _ = self.execute("resume")
        self.assertEqual(self.mutations(), [])
        self.assertIs(self.state()["finalized"], expr2=False)

    def test_missing_container_after_reboot_is_not_recreated(self) -> None:
        """Missing container after reboot is not recreated."""
        _ = self.execute("prepare")
        self.current_boot = NEW_BOOT
        self.containers["postgres"] = None
        self.calls.clear()
        with self.assertRaisesRegex(public.ReleaseError, "Exactly one existing postgres container"):
            _ = self.execute("resume")
        self.assertEqual(self.mutations(), [])
        self.assertIs(self.state()["finalized"], expr2=False)

    def test_changed_resolved_environment_after_reboot_prevents_any_start(self) -> None:
        """Changed resolved environment after reboot prevents any start."""
        _ = self.execute("prepare")
        self.current_boot = NEW_BOOT
        obj(self.resolved, "services", "simplestchat", "environment")["RUN_MIGRATIONS"] = "true"
        self.calls.clear()
        with self.assertRaisesRegex(public.ReleaseError, "Resolved configuration changed"):
            _ = self.execute("resume")
        self.assertEqual(self.mutations(), [])

    def test_unfinished_preparation_cannot_be_started_again(self) -> None:
        """Unfinished preparation cannot be started again."""
        _ = self.execute("prepare")
        self.calls.clear()
        with self.assertRaises(public.ReleaseError):
            _ = self.execute("prepare")
        self.assertEqual(self.calls, [])
        self.assertEqual(self.state()["phase"], "await_reboot")

    def test_database_readiness_failure_prevents_app_and_proxy_start_and_automatic_retry(
        self,
    ) -> None:
        """Database readiness failure prevents app and proxy start and automatic retry."""
        _ = self.execute("prepare")
        self.current_boot = NEW_BOOT
        self.unhealthy_resume = True
        self.calls.clear()
        with self.assertRaisesRegex(public.ReleaseError, "Database did not become healthy"):
            _ = self.execute("resume")
        self.assertEqual([args[-1] for args in self.mutations()], ["postgres"])
        self.assertIs(self.state()["finalized"], expr2=False)
        self.assertIs(self.report()["passed"], expr2=False)
        self.calls.clear()
        with self.assertRaises(public.ReleaseError):
            _ = self.execute("resume")
        self.assertEqual(self.calls, [])

    def test_app_readiness_failure_prevents_proxy_start(self) -> None:
        """App readiness failure prevents proxy start."""
        _ = self.execute("prepare")
        self.current_boot = NEW_BOOT
        self.unready = True
        self.calls.clear()
        with self.assertRaisesRegex(public.ReleaseError, "readiness deadline"):
            _ = self.execute("resume")
        self.assertEqual([args[-1] for args in self.mutations()], ["postgres", "simplestchat"])
        self.assertIs(self.report()["passed"], expr2=False)

    def test_trusted_https_is_required_before_success(self) -> None:
        """Trusted https is required before success."""
        _ = self.execute("prepare")
        self.current_boot = NEW_BOOT
        self.proxy_unready = True
        with self.assertRaisesRegex(public.ReleaseError, "readiness deadline"):
            _ = self.execute("resume")
        self.assertIs(self.state()["finalized"], expr2=False)
        self.assertIs(self.report()["passed"], expr2=False)

    def test_unfinished_benchmark_blocks_prepare_and_post_reboot_recovery(self) -> None:
        """Unfinished benchmark blocks prepare and post reboot recovery."""
        public.atomic(self.work / "current.json", {"schemaVersion": 1, "finalized": False})
        with self.assertRaises(public.ReleaseError):
            _ = self.execute("prepare")
        self.assertEqual(self.calls, [])
        public.atomic(self.work / "current.json", {"schemaVersion": 1, "finalized": True})
        _ = self.execute("prepare")
        self.current_boot = NEW_BOOT
        public.atomic(
            self.work / "current.json",
            {"schemaVersion": 1, "finalized": False, "phase": "await_reboot", "bootId": OLD_BOOT},
        )
        self.calls.clear()
        with self.assertRaises(public.ReleaseError):
            _ = self.execute("resume")
        self.assertEqual(self.calls, [])

    def test_recovery_rejects_unprotected_or_tampered_evidence(self) -> None:
        """Recovery rejects unprotected or tampered evidence."""
        _ = self.execute("prepare")
        self.current_boot = NEW_BOOT
        self.calls.clear()
        snapshot = self.attempt() / "identity.json"
        snapshot.chmod(0o644)
        with self.assertRaisesRegex(public.ReleaseError, "permission"):
            _ = self.execute("resume")
        snapshot.chmod(0o600)
        _ = snapshot.write_text("{}")
        with self.assertRaisesRegex(public.ReleaseError, "evidence changed"):
            _ = self.execute("resume")
        self.assertEqual(self.calls, [])

    def test_recovery_rejects_attempt_path_outside_private_results(self) -> None:
        """Recovery rejects attempt path outside private results."""
        _ = self.execute("prepare")
        self.current_boot = NEW_BOOT
        record = self.state()
        record["attempt"] = str(self.root / "reboot.unrelated")
        public.atomic(self.root / "release-state.json", record)
        self.calls.clear()
        with self.assertRaisesRegex(public.ReleaseError, "attempt path"):
            _ = self.execute("resume")
        self.assertEqual(self.calls, [])

    def test_finalized_reboot_cannot_be_resumed_again(self) -> None:
        """Finalized reboot cannot be resumed again."""
        _ = self.execute("prepare")
        self.current_boot = NEW_BOOT
        _ = self.execute("resume")
        self.calls.clear()
        with self.assertRaisesRegex(public.ReleaseError, "No unfinished prepared reboot"):
            _ = self.execute("resume")
        self.assertEqual(self.calls, [])


@final
class RebootPlaybookTests(unittest.TestCase):
    """Require explicit maintenance authorization around every existing-container reboot."""

    def test_explicit_authorization_and_existing_host_guards_precede_reboot(self) -> None:
        """Explicit authorization and existing host guards precede reboot."""
        play = obj(yaml_value((FILES.parent / "reboot.yml").read_text()), 0)
        self.assertEqual(play["serial"], 1)
        guard = obj(play, "pre_tasks", 0)
        self.assertEqual(guard["tags"], ["always"])
        self.assertIn(
            "scpub_reboot | default(false) | bool", strings(guard, "ansible.builtin.assert", "that")
        )
        self.assertIn("scpub_enabled | bool", strings(guard, "ansible.builtin.assert", "that"))
        tasks = objects(play, "tasks")
        workflow = tasks[-1]
        preparation = obj(workflow, "block", 0)
        self.assertEqual(at(preparation, "ansible.builtin.command", "argv", -1), "prepare")
        self.assertEqual(workflow["when"], "not ansible_check_mode")
        self.assertIn("ansible.builtin.reboot", obj(workflow, "block", 1))
        self.assertEqual(at(workflow, "block", 2, "ansible.builtin.command", "argv", -1), "resume")
        self.assertEqual(at(workflow, "rescue", 0, "ansible.builtin.command", "argv", -1), "cancel")
        self.assertIs(at(workflow, "rescue", 0, "failed_when"), expr2=False)
        self.assertIn("ansible.builtin.fail", obj(workflow, "rescue", -1))
        for task in (preparation, obj(workflow, "block", 2), obj(workflow, "rescue", 0)):
            argv = strings(task, "ansible.builtin.command", "argv")
            self.assertIn("--property=RuntimeMaxSec=900", argv)
            self.assertIn("--wait", argv)
        self.assertFalse(
            any(
                "ansible.builtin.apt" in task or "ansible.builtin.service" in task for task in tasks
            )
        )


if __name__ == "__main__":
    _ = unittest.main()
