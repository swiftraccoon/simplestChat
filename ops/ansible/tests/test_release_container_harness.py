"""Offline refusal, evidence and owned-cleanup checks; never contact Docker."""

from __future__ import annotations

import io
import json
import os
import platform
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import unittest
from contextlib import ExitStack, redirect_stderr, redirect_stdout
from copy import deepcopy
from dataclasses import dataclass
from dataclasses import field as dataclass_field
from pathlib import Path
from typing import TYPE_CHECKING, Protocol, Unpack, override
from unittest.mock import patch

from test_support import ROOT

# isort: split
import release_container_harness as harness_module
import release_public as release
from release_artifact import sha256_file
from release_container_fixture import LABEL
from release_json import JsonObject, JsonValue, decode_json, object_value, string_value

if TYPE_CHECKING:
    from collections.abc import Iterable

    from release_container_harness import CommandOptions

TOKEN = "c" * 32
CONTAINER = "a" * 64
NETWORK = "b" * 64
IMAGE = "sha256:" + "d" * 64
FAILED_IMAGE = "sha256:" + "e" * 64
REVISION = "f" * 40


@dataclass
class TextResult:
    """Bounded in-memory command output without any child process."""

    value: str = ""
    code: int = 0
    error: Path = Path("/missing-offline-stderr")

    def text(self) -> str:
        """Return the explicitly supplied output."""
        return self.value


@dataclass(frozen=True)
class DockerCall:
    """One explicit Docker operation and its bounded command options."""

    args: tuple[str, ...]
    kwargs: CommandOptions


class DockerHandler(Protocol):
    """A typed callback implementing one expected Docker response."""

    def __call__(self, *args: str, **options: Unpack[CommandOptions]) -> TextResult:
        """Handle one recorded call without invoking Docker."""
        ...


class RunHandler(Protocol):
    """A typed callback implementing one expected host command response."""

    def __call__(self, args: list[str], /, **options: Unpack[CommandOptions]) -> TextResult:
        """Handle one recorded call without invoking a host command."""
        ...


@dataclass
class FakeDocker:
    """Record exact typed calls, then use only explicit in-memory responses."""

    return_value: TextResult = dataclass_field(default_factory=TextResult)
    side_effect: DockerHandler | list[TextResult] | None = None
    call_args_list: list[DockerCall] = dataclass_field(default_factory=list)

    @property
    def call_count(self) -> int:
        """Count invocations without a dynamically typed mock."""
        return len(self.call_args_list)

    def __call__(self, *args: str, **options: Unpack[CommandOptions]) -> TextResult:
        """Record the immutable argv before selecting the expected response."""
        self.call_args_list.append(DockerCall(args, options))
        if isinstance(self.side_effect, list):
            return self.side_effect.pop(0)
        if self.side_effect is not None:
            return self.side_effect(*args, **options)
        return self.return_value

    def assert_not_called(self) -> None:
        """Require refusal to precede any Docker interaction."""
        if self.call_args_list:
            message = "Unexpected Docker interaction"
            raise AssertionError(message)


@dataclass
class FakeRun:
    """Record explicit host commands using a statically typed callback."""

    side_effect: RunHandler | None = None

    def __call__(self, args: list[str], **options: Unpack[CommandOptions]) -> TextResult:
        """Return only the configured command response."""
        if self.side_effect is not None:
            return self.side_effect(args, **options)
        return TextResult()


@dataclass
class FakeCommands:
    """Offline runner implementing the complete harness command protocol."""

    docker: FakeDocker = dataclass_field(default_factory=FakeDocker)
    run: FakeRun = dataclass_field(default_factory=FakeRun)
    compose: FakeDocker = dataclass_field(default_factory=FakeDocker)
    settlement_unconfirmed: bool = False
    last_failure: JsonObject | None = None


@dataclass
class FakeLifecycle:
    """Exercise main's original-outcome bookkeeping without starting fixture services."""

    execution_passes: bool
    cleanup_passes: bool
    commands: FakeCommands = dataclass_field(default_factory=FakeCommands)
    report: JsonObject = dataclass_field(
        default_factory=lambda: {
            "schemaVersion": 1,
            "passed": False,
            "cleanupPassed": False,
            "phase": "preflight",
        }
    )
    cleanup_calls: int = 0

    def setup(self) -> None:
        """Avoid every host preflight and fixture mutation."""

    def exercise(self) -> None:
        """Produce the one explicitly selected original execution result."""
        if not self.execution_passes:
            message = "Original execution failure"
            raise harness_module.CheckError(message)

    def cleanup(self) -> None:
        """Record exactly one cleanup attempt and its independent outcome."""
        self.cleanup_calls += 1
        self.report["cleanupPassed"] = self.cleanup_passes


@dataclass
class FakeChild:
    """Model child completion or interruption without allocating a process."""

    returncode: int | None = 42
    interruption: Exception | None = None

    def wait(self, timeout: float | None = None) -> int:
        """Produce the selected original status within the modeled timeout."""
        _ = timeout
        if self.interruption is not None:
            raise self.interruption
        if self.returncode is None:
            message = "Fake child has no completion status"
            raise AssertionError(message)
        return self.returncode

    def poll(self) -> int | None:
        """Distinguish completed children from interrupted, unsettled ones."""
        return self.returncode


@dataclass
class FakeLaunch:
    """Capture subprocess options through an object-typed external API boundary."""

    child: FakeChild
    options: dict[str, object] = dataclass_field(default_factory=dict)

    def __call__(self, *_args: object, **options: object) -> FakeChild:
        """Return only the precreated in-memory child."""
        self.options = options
        return self.child


def read_object(data: str | bytes) -> JsonObject:
    """Validate fixture evidence at the same strict JSON boundary as production."""
    return object_value(decode_json(data))


def outcome_command(
    testcase: unittest.TestCase, root: Path, outcome: JsonObject, code: int
) -> RunHandler:
    """Bind a release response to one loop case without dynamically typed callbacks."""

    def invoke(args: list[str], **kwargs: Unpack[CommandOptions]) -> TextResult:
        testcase.assertEqual(
            args[-3:], [str(harness_module.FILES / "release-public.py"), "deploy", REVISION]
        )
        testcase.assertEqual(kwargs, {"timeout": 180, "success": False})
        attempt = root / "results" / "release.owned"
        attempt.mkdir()
        _ = (attempt / "outcome.json").write_text(json.dumps(outcome), encoding="utf-8")
        return TextResult(code=code)

    return invoke


def failed_inspection(harness: harness_module.Harness, response: TextResult) -> DockerHandler:
    """Bind a secondary inspection failure independently of the test loop's next case."""

    def inspect(*_args: str, **_kwargs: Unpack[CommandOptions]) -> TextResult:
        harness.commands.last_failure = {
            "number": 57,
            "operation": "docker inspect",
            "exitStatus": 1,
        }
        return response

    return inspect


def fake_commands(harness: harness_module.Harness) -> FakeCommands:
    """Narrow the fixture's explicitly installed recorder before asserting calls."""
    if not isinstance(harness.commands, FakeCommands):
        message = "Expected the typed offline recorder"
        raise TypeError(message)
    return harness.commands


def cleanup_commands(harness: harness_module.Harness) -> CleanupCommands:
    """Narrow the strict cleanup model before inspecting its retained inventory."""
    if not isinstance(harness.commands, CleanupCommands):
        message = "Expected the typed cleanup inventory"
        raise TypeError(message)
    return harness.commands


def owned(
    identity: str = CONTAINER,
    service: str = "simplestchat",
    *,
    running: bool = True,
    image: str = IMAGE,
    exit_code: int = 0,
) -> JsonObject:
    """Return one complete, labeled container inspection document."""
    return {
        "id": identity,
        "image": image,
        "restarts": 0,
        "state": {
            "Running": running,
            "ExitCode": exit_code,
            "OOMKilled": False,
            "Error": "",
            "StartedAt": "2026-09-13T00:00:00Z",
            "Pid": 42 if running else 0,
        },
        "labels": {
            LABEL: TOKEN,
            "com.docker.compose.project": "simplestchat-public",
            "com.docker.compose.service": service,
        },
    }


class CleanupCommands:
    """Model only the exact local inventory and single-ID cleanup operations."""

    def __init__(
        self,
        containers: Iterable[JsonObject] = (),
        networks: Iterable[JsonObject] = (),
        *,
        remaining: str = "",
        update_failure: bool = False,
    ) -> None:
        """Own an independent inventory so tests can detect retained resources."""
        self.containers: dict[str, JsonObject] = {
            string_value(value["id"]): deepcopy(value) for value in containers
        }
        self.networks: dict[str, JsonObject] = {
            string_value(value["Id"]): deepcopy(value) for value in networks
        }
        self.remaining: str = remaining
        self.update_failure: bool = update_failure
        self.calls: list[tuple[tuple[str, ...], CommandOptions]] = []
        self.settlement_unconfirmed: bool = False
        self.last_failure: JsonObject | None = None

    def docker(self, *args: str, **kwargs: Unpack[CommandOptions]) -> TextResult:
        """Refuse every command outside the exact cleanup inventory operations."""
        self.calls.append((args, kwargs))
        if args[0] == "network":
            return self.network(args)
        if args[0] == "ps":
            allowed = {"--all", "--quiet", "--no-trunc", "--filter", f"label={LABEL}={TOKEN}"}
            if not set(args[1:]).issubset(allowed):
                msg = f"Unexpected container inventory: {args}"
                raise AssertionError(msg)
            return TextResult("\n".join(self.containers) if "--filter" in args else self.remaining)
        if args[0] == "inspect":
            return TextResult(json.dumps(self.containers[args[-1]]))
        if args[0] == "logs":
            if args != ("logs", "--tail", "150", args[-1]) or args[-1] not in self.containers:
                msg = f"Unexpected log capture: {args}"
                raise AssertionError(msg)
            return TextResult()
        if args[0] == "stop":
            if args != ("stop", "--time", "30", args[-1]) or kwargs != {"timeout": 45}:
                msg = f"Unexpected stop: {args}, {kwargs}"
                raise AssertionError(msg)
            object_value(self.containers[args[-1]]["state"]).update({"Running": False, "Pid": 0})
            return TextResult()
        if args == ("rm", args[-1]):
            del self.containers[args[-1]]
            return TextResult()
        msg = f"Unexpected Docker mutation: {args}"
        raise AssertionError(msg)

    def network(self, args: tuple[str, ...]) -> TextResult:
        """Model only label-filtered network inventory and exact-ID removal."""
        if args[:2] == ("network", "ls"):
            allowed = {"--quiet", "--no-trunc", "--filter", f"label={LABEL}={TOKEN}"}
            if not set(args[2:]).issubset(allowed):
                message = f"Unexpected network inventory: {args}"
                raise AssertionError(message)
            return TextResult("\n".join(self.networks))
        if args[:2] == ("network", "inspect"):
            return TextResult(json.dumps(self.networks[args[-1]]))
        if args == ("network", "rm", args[-1]):
            del self.networks[args[-1]]
            return TextResult()
        msg = f"Unexpected network mutation: {args}"
        raise AssertionError(msg)

    def run(self, args: list[str], **kwargs: Unpack[CommandOptions]) -> TextResult:
        """Allow only the single expected trust-store refresh."""
        self.calls.append((tuple(args), kwargs))
        if args != ["/usr/sbin/update-ca-certificates"] or kwargs != {"timeout": 30}:
            msg = f"Unexpected host command: {args}, {kwargs}"
            raise AssertionError(msg)
        if self.update_failure:
            msg = "Owned fixture trust-store refresh failed"
            raise harness_module.CheckError(msg)
        return TextResult()

    def compose(self, *args: str, **kwargs: Unpack[CommandOptions]) -> TextResult:
        """Forbid Compose mutations in exact-ID cleanup."""
        message = f"Unexpected Compose command: {args}, {kwargs}"
        raise AssertionError(message)


class HarnessTestCase(unittest.TestCase):
    """Allocate isolated private files and forbid real child processes in every test."""

    def __init__(self, method_name: str = "runTest") -> None:
        """Register temporary cleanup even when a test setup or assertion fails."""
        super().__init__(method_name)
        temporary = tempfile.TemporaryDirectory(prefix="simplestchat-release-harness.")
        self.addCleanup(temporary.cleanup)
        self.directory: Path = Path(temporary.name).resolve()

    @override
    def setUp(self) -> None:
        """Prevent accidental host commands while allowing narrower typed child fixtures."""
        # An unexpected implementation change must not contact a daemon or
        # execute a host command from this offline suite.
        process = patch.object(
            subprocess, "Popen", side_effect=AssertionError("No child commands in offline tests")
        )
        _ = process.start()
        self.addCleanup(process.stop)

    def harness(self) -> harness_module.Harness:
        """Create a no-side-effect harness with a deterministic ownership identity."""
        self.assertEqual(
            Path(harness_module.__file__).resolve(), ROOT / "build/release_container_harness.py"
        )
        output = self.directory / f"output-{len(list(self.directory.iterdir()))}"
        output.mkdir(mode=0o700)
        (output / "private").mkdir(mode=0o700)
        harness = harness_module.Harness(output, "simplestchat-ci:production")
        harness.token = TOKEN
        harness.fixture_images["failed"] = {"image": FAILED_IMAGE}
        return harness


class HarnessInputTests(HarnessTestCase):
    """Keep CLI opt-in and private output allocation independent of service effects."""

    def test_socket_directory_remains_traversable_under_private_process_umask(self) -> None:
        """Retain the shared socket traversal mode despite the restrictive parent umask."""
        initial_umask = os.umask(0o077)
        try:
            with patch.object(os, "chown") as chown:
                for name, uid, mode in (
                    ("postgres", 999, 0o700),
                    ("postgres-socket", 999, 0o755),
                    ("caddy-data", 10001, 0o700),
                    ("caddy-config", 10001, 0o700),
                ):
                    path = self.directory / name
                    harness_module.create_data_directory(path, uid, mode)
                    self.assertEqual(stat.S_IMODE(path.stat().st_mode), mode)
                    chown.assert_called_with(path, uid, uid)
                with self.assertRaises(FileExistsError):
                    harness_module.create_data_directory(
                        self.directory / "postgres-socket", 999, 0o755
                    )
        finally:
            _ = os.umask(initial_umask)

    def test_cli_requires_explicit_disposable_opt_in_and_all_inputs(self) -> None:
        """Require deliberate host opt-in, a local image, and a fresh evidence path."""
        valid = [
            "--disposable-host",
            "--image",
            "simplestchat-ci:production",
            "--output",
            str(self.directory / "fresh-output"),
        ]
        for arguments in (valid[1:], valid[:1] + valid[3:], valid[:3]):
            with (
                self.subTest(arguments=arguments),
                redirect_stderr(io.StringIO()),
                self.assertRaises(SystemExit) as error,
            ):
                _ = harness_module.options(arguments)
            self.assertEqual(error.exception.code, 2)
        arguments = harness_module.options(valid)
        self.assertIs(arguments.disposable_host, expr2=True)
        self.assertEqual(arguments.image, "simplestchat-ci:production")

    def test_cli_refuses_image_options_shell_text_whitespace_and_oversized_selectors(self) -> None:
        """Reject image selectors that could become flags or ambiguous command arguments."""
        for image in (
            "",
            "--privileged",
            "image;true",
            "image\nother",
            "image with space",
            "$(id)",
            "x" * 257,
        ):
            with (
                self.subTest(image=image),
                redirect_stderr(io.StringIO()),
                self.assertRaises(SystemExit),
            ):
                _ = harness_module.options(
                    [
                        "--disposable-host",
                        "--image=" + image,
                        "--output",
                        str(self.directory / "new"),
                    ]
                )
        self.assertEqual(list(self.directory.iterdir()), [])

    def test_output_is_new_and_private_before_any_host_side_effect(self) -> None:
        """Reserve private evidence exactly once without touching fixture services."""
        output = self.directory / "report"
        self.assertEqual(harness_module.fresh_output(str(output)), output)
        self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE((output / "private").stat().st_mode), 0o700)
        self.assertEqual(list(output.iterdir()), [output / "private"])
        with self.assertRaises(harness_module.CheckError):
            _ = harness_module.fresh_output(str(output))

    def test_existing_file_symlink_parent_and_dangling_output_are_preserved(self) -> None:
        """Preserve every preexisting or ambiguously resolved output target."""
        existing = self.directory / "existing"
        _ = existing.write_text("retained evidence")
        linked = self.directory / "link"
        linked.symlink_to(self.directory, target_is_directory=True)
        dangling = self.directory / "dangling"
        dangling.symlink_to(self.directory / "missing")
        for output in (existing, linked / "new", dangling):
            with self.subTest(output=output), self.assertRaises(harness_module.CheckError):
                _ = harness_module.fresh_output(str(output))
        self.assertEqual(existing.read_text(encoding="utf-8"), "retained evidence")
        self.assertTrue(dangling.is_symlink())
        self.assertFalse((self.directory / "new").exists())

    def test_output_rejects_relative_missing_parent_controls_and_fixture_storage(self) -> None:
        """Keep evidence outside fixture storage and require an unambiguous absolute path."""
        fixed = self.directory / "public"
        with patch.object(harness_module, "FIXED_PATHS", (fixed,)):
            for output in (
                "relative-output",
                str(self.directory / "missing" / "child"),
                str(self.directory / "bad\nname"),
                str(fixed),
            ):
                with self.subTest(output=output), self.assertRaises(harness_module.CheckError):
                    _ = harness_module.fresh_output(output)
            fixed.mkdir()
            with self.assertRaises(harness_module.CheckError):
                _ = harness_module.fresh_output(str(fixed / "child"))
        self.assertEqual(list(fixed.iterdir()), [])


class HarnessPreflightTests(HarnessTestCase):
    """Refuse unsuitable hosts before daemon access or fixed-path creation."""

    def test_base_image_inspection_handles_only_optional_entrypoint_with_map_lookup(self) -> None:
        """Inspect optional entrypoint metadata without dereferencing a missing field."""
        harness = self.harness()
        harness.commands = FakeCommands()
        replies: dict[tuple[str, ...], str] = {
            ("ps", "--all", "--quiet"): "",
            (
                "network",
                "ls",
                "--quiet",
                "--filter",
                "label=com.docker.compose.project=simplestchat-public",
            ): "",
            (
                "image",
                "save",
                "--help",
            ): "Options:\n      --platform string   Export a specific platform\n",
            ("version", "--format", "{{.Server.APIVersion}}"): "1.55",
            ("buildx", "inspect", "default"): "Name: default\nDriver: docker\n",
        }

        class InspectionCapturedError(Exception):
            pass

        def inspect(*args: str, **kwargs: Unpack[CommandOptions]) -> TextResult:
            self.assertEqual(kwargs, {})
            if args[:2] == ("image", "inspect"):
                self.assertEqual(
                    args,
                    (
                        "image",
                        "inspect",
                        "--format",
                        '{"id":{{json .Id}},"os":{{json .Os}},'
                        + '"architecture":{{json .Architecture}},"user":{{json .Config.User}},'
                        + '"cmd":{{json .Config.Cmd}},'
                        + '"entrypoint":{{json (index .Config "Entrypoint")}}}',
                        "simplestchat-ci:production",
                    ),
                )
                raise InspectionCapturedError
            self.assertIn(args, replies, "Stop before any fixture image or host mutation")
            return TextResult(replies[args])

        fake_commands(harness).docker.side_effect = inspect
        with (
            patch.object(harness_module, "host_preflight"),
            self.assertRaises(InspectionCapturedError),
        ):
            harness.setup()
        self.assertEqual(fake_commands(harness).docker.call_count, 6)
        self.assertIs(harness.created_paths, expr2=False)

    def test_wrong_platform_architecture_or_uid_refuses_before_tool_or_socket_checks(self) -> None:
        """Reject the wrong host platform or privilege before any external probing."""
        for platform_name, machine, uid in (
            ("darwin", "arm64", 0),
            ("linux", "aarch64", 0),
            ("linux", "x86_64", 501),
        ):
            with (
                self.subTest(platform=platform_name, machine=machine, uid=uid),
                ExitStack() as stack,
            ):
                _ = stack.enter_context(patch.object(sys, "platform", platform_name))
                _ = stack.enter_context(patch.object(platform, "machine", return_value=machine))
                _ = stack.enter_context(patch.object(os, "geteuid", return_value=uid))
                access = stack.enter_context(patch.object(os, "access"))
                sockets = stack.enter_context(patch.object(socket, "socket"))
                with self.assertRaisesRegex(harness_module.CheckError, "fresh disposable Linux"):
                    harness_module.host_preflight()
                access.assert_not_called()
                sockets.assert_not_called()

    def test_existing_public_or_benchmark_path_refuses_without_contacting_services(self) -> None:
        """Treat existing fixture paths, including dangling symlinks, as a hard refusal."""
        for dangling in (False, True):
            existing = self.directory / ("dangling" if dangling else "public")
            if dangling:
                existing.symlink_to(self.directory / "missing")
            else:
                existing.mkdir()
            with self.subTest(dangling=dangling), ExitStack() as stack:
                _ = stack.enter_context(patch.object(sys, "platform", "linux"))
                _ = stack.enter_context(patch.object(platform, "machine", return_value="x86_64"))
                _ = stack.enter_context(patch.object(os, "geteuid", return_value=0))
                _ = stack.enter_context(patch.object(harness_module, "FIXED_PATHS", (existing,)))
                access = stack.enter_context(patch.object(os, "access"))
                sockets = stack.enter_context(patch.object(socket, "socket"))
                with self.assertRaisesRegex(
                    harness_module.CheckError, "Existing service or benchmark path"
                ):
                    harness_module.host_preflight()
                access.assert_not_called()
                sockets.assert_not_called()
            self.assertTrue(existing.is_symlink() if dangling else existing.is_dir())

    def test_missing_tooling_fails_before_port_checks(self) -> None:
        """Require all fixed host tools before checking service port availability."""
        with (
            patch.object(sys, "platform", "linux"),
            patch.object(platform, "machine", return_value="x86_64"),
            patch.object(os, "geteuid", return_value=0),
            patch.object(harness_module, "FIXED_PATHS", ()),
            patch.object(os, "access", return_value=False),
            patch.object(socket, "socket") as sockets,
        ):
            with self.assertRaisesRegex(harness_module.CheckError, "tooling is missing"):
                harness_module.host_preflight()
            sockets.assert_not_called()


class HarnessOwnershipTests(HarnessTestCase):
    """Restrict teardown to exact identities with verified ownership and clean settlement."""

    def test_unconfirmed_command_settlement_forbids_all_cleanup_mutations(self) -> None:
        """Retain containers and trust material when command settlement is uncertain."""
        harness = self.harness()
        harness.created_paths = True
        harness.commands = CleanupCommands([owned()])
        harness.commands.settlement_unconfirmed = True
        ca = self.directory / "retained-trust.crt"
        _ = ca.write_text("owned fixture certificate")
        harness.ca_path, harness.ca_digest = ca, sha256_file(ca)
        harness.cleanup()
        self.assertIs(harness.report["cleanupPassed"], expr2=False)
        self.assertEqual(harness.report["cleanupFailures"], ["CommandSettlementUnconfirmed"])
        self.assertEqual(cleanup_commands(harness).calls, [])
        self.assertEqual(ca.read_text(encoding="utf-8"), "owned fixture certificate")

    def test_incomplete_ca_publication_is_reported_and_never_deleted(self) -> None:
        """Never infer ownership of a partially published trust entry."""
        harness = self.harness()
        harness.commands = CleanupCommands()
        ca = self.directory / "partial-trust.crt"
        _ = ca.write_text("partial certificate")
        harness.ca_path = ca
        self.assertIsNone(harness.ca_digest)
        harness.cleanup()
        self.assertIs(harness.report["cleanupPassed"], expr2=False)
        self.assertEqual(harness.report["cleanupFailures"], ["FixtureCaPublicationIncomplete"])
        self.assertEqual(cleanup_commands(harness).calls, [])
        self.assertEqual(ca.read_text(encoding="utf-8"), "partial certificate")

    def test_container_identity_must_be_full_length_before_any_inspection(self) -> None:
        """Reject names, short IDs, and injected options before asking Docker anything."""
        commands = FakeCommands()
        for identity in ("a" * 12, "sha256:" + CONTAINER, CONTAINER + "\n", "--all", ""):
            with self.subTest(identity=identity), self.assertRaises(harness_module.CheckError):
                _ = harness_module.inspect_owned(commands, identity, TOKEN)
        commands.docker.assert_not_called()

    def test_exact_identity_and_all_ownership_labels_are_required(self) -> None:
        """Require the exact container ID and every fixture ownership label."""
        for field, replacement in (
            ("id", "b" * 64),
            (LABEL, "other-fixture"),
            ("com.docker.compose.project", "unrelated"),
            ("com.docker.compose.service", "other"),
        ):
            value = owned()
            if field == "id":
                value[field] = replacement
            else:
                object_value(value["labels"])[field] = replacement
            commands = FakeCommands()
            commands.docker.return_value = TextResult(json.dumps(value))
            with self.subTest(field=field), self.assertRaises(harness_module.CheckError):
                _ = harness_module.inspect_owned(commands, CONTAINER, TOKEN)
        commands = FakeCommands()
        commands.docker.return_value = TextResult(json.dumps(owned()))
        self.assertEqual(harness_module.inspect_owned(commands, CONTAINER, TOKEN), owned())

    def test_healthy_exit_distinguishes_the_deliberate_failure_from_real_damage(self) -> None:
        """Accept only clean exits or the explicitly selected failure-image exit."""
        harness_module.assert_healthy_exit(owned(running=False))
        harness_module.assert_healthy_exit(
            owned(running=False, exit_code=42), deliberate_failure=True
        )
        for field, replacement in (
            ("Running", True),
            ("OOMKilled", True),
            ("Error", "daemon failure"),
            ("ExitCode", 137),
        ):
            value = owned(running=False)
            object_value(value["state"])[field] = replacement
            with self.subTest(field=field), self.assertRaises(harness_module.CheckError):
                harness_module.assert_healthy_exit(value)
        with self.assertRaises(harness_module.CheckError):
            harness_module.assert_healthy_exit(owned(running=False, exit_code=42))

    def test_cleanup_verifies_every_container_before_stopping_or_removing_any(self) -> None:
        """Validate the complete selected inventory before mutating its first member."""
        harness = self.harness()
        harness.created_paths = True
        wrong = owned("e" * 64, "postgres")
        object_value(wrong["labels"])[LABEL] = "another-run"
        harness.commands = CleanupCommands([owned(), wrong])
        harness.cleanup()
        self.assertIs(harness.report["cleanupPassed"], expr2=False)
        self.assertEqual(harness.report["cleanupFailures"], ["CheckError"])
        self.assertFalse(
            any(args[0] in ("stop", "rm", "logs") for args, _ in cleanup_commands(harness).calls)
        )

    def test_cleanup_uses_only_verified_ids_in_service_shutdown_order(self) -> None:
        """Stop the application before dependencies and remove only exact owned IDs."""
        harness = self.harness()
        harness.created_paths = True
        containers = [
            owned("4" * 64, "postgres"),
            owned("3" * 64, "caddy"),
            owned("2" * 64, "migrate"),
            owned("1" * 64, "simplestchat"),
        ]
        network: JsonObject = {
            "Id": NETWORK,
            "Labels": {LABEL: TOKEN, "com.docker.compose.project": "simplestchat-public"},
            "Containers": {},
        }
        harness.commands = CleanupCommands(containers, [network])
        harness.cleanup()
        self.assertIs(harness.report["cleanupPassed"], expr2=True)
        self.assertEqual(
            [args[-1] for args, _ in cleanup_commands(harness).calls if args[0] == "stop"],
            [character * 64 for character in "1234"],
        )
        self.assertEqual(
            [args for args, _ in cleanup_commands(harness).calls if args[0] == "rm"],
            [("rm", character * 64) for character in "1234"],
        )
        self.assertIn((("network", "rm", NETWORK), {}), cleanup_commands(harness).calls)
        inventories = [args for args, _ in cleanup_commands(harness).calls if "--filter" in args]
        self.assertEqual(len(inventories), 2)
        self.assertTrue(all("--no-trunc" in args for args in inventories))
        self.assertNotIn("prune", str(cleanup_commands(harness).calls))
        self.assertNotIn("--force", str(cleanup_commands(harness).calls))

    def test_unexpected_exit_preserves_container_and_marks_cleanup_failed(self) -> None:
        """Retain an unexpectedly failed container for private investigation."""
        harness = self.harness()
        harness.created_paths = True
        harness.commands = CleanupCommands([owned(running=False, exit_code=137)])
        harness.cleanup()
        self.assertIs(harness.report["cleanupPassed"], expr2=False)
        self.assertIn(CONTAINER, cleanup_commands(harness).containers)
        self.assertFalse(any(args[0] == "rm" for args, _ in cleanup_commands(harness).calls))

    def test_deliberate_failed_image_can_be_removed_only_with_expected_exit(self) -> None:
        """Permit cleanup of the expected failed fixture without masking other exits."""
        harness = self.harness()
        harness.created_paths = True
        harness.commands = CleanupCommands([owned(running=False, image=FAILED_IMAGE, exit_code=42)])
        harness.cleanup()
        self.assertIs(harness.report["cleanupPassed"], expr2=True)
        self.assertNotIn(CONTAINER, cleanup_commands(harness).containers)

    def test_network_with_other_ownership_or_attached_containers_is_retained(self) -> None:
        """Retain networks unless ownership and absence of attachments are both certain."""
        for wrong_owner in (False, True):
            harness = self.harness()
            harness.created_paths = True
            network: JsonObject = {
                "Id": NETWORK,
                "Labels": {
                    LABEL: "other" if wrong_owner else TOKEN,
                    "com.docker.compose.project": "simplestchat-public",
                },
                "Containers": {} if wrong_owner else {CONTAINER: {"Name": "still-running"}},
            }
            harness.commands = CleanupCommands([], [network])
            harness.cleanup()
            self.assertIs(harness.report["cleanupPassed"], expr2=False)
            self.assertIn(NETWORK, cleanup_commands(harness).networks)
            self.assertNotIn((("network", "rm", NETWORK), {}), cleanup_commands(harness).calls)

    def test_unowned_container_remaining_is_reported_without_deleting_it(self) -> None:
        """Report leftover unowned containers without expanding the deletion scope."""
        harness = self.harness()
        harness.created_paths = True
        harness.commands = CleanupCommands(remaining="9" * 64)
        harness.cleanup()
        self.assertIs(harness.report["cleanupPassed"], expr2=False)
        self.assertFalse(
            any(args[0] in ("stop", "rm") for args, _ in cleanup_commands(harness).calls)
        )

    def test_no_created_fixture_means_no_container_or_host_commands(self) -> None:
        """Avoid all teardown commands when setup created no fixture resources."""
        harness = self.harness()
        harness.commands = CleanupCommands()
        harness.cleanup()
        self.assertIs(harness.report["cleanupPassed"], expr2=True)
        self.assertEqual(cleanup_commands(harness).calls, [])

    def test_ca_cleanup_requires_unchanged_regular_file_and_refreshes_trust(self) -> None:
        """Remove only the unchanged regular CA file and refresh trust exactly once."""
        harness = self.harness()
        harness.commands = CleanupCommands()
        ca = self.directory / "owned-ca.crt"
        _ = ca.write_text("owned fixture certificate")
        harness.ca_path, harness.ca_digest = ca, sha256_file(ca)
        harness.cleanup()
        self.assertIs(harness.report["cleanupPassed"], expr2=True)
        self.assertFalse(ca.exists())
        self.assertEqual(
            cleanup_commands(harness).calls,
            [(("/usr/sbin/update-ca-certificates",), {"timeout": 30})],
        )

    def test_changed_or_symlinked_ca_is_never_removed(self) -> None:
        """Retain changed or symlinked trust entries rather than guessing ownership."""
        for symlink in (False, True):
            harness = self.harness()
            harness.commands = CleanupCommands()
            target = self.directory / f"certificate-{symlink}"
            _ = target.write_text("owned fixture certificate")
            harness.ca_digest = sha256_file(target)
            ca = self.directory / f"trust-{symlink}"
            if symlink:
                ca.symlink_to(target)
            else:
                _ = ca.write_text("different certificate")
            harness.ca_path = ca
            harness.cleanup()
            self.assertIs(harness.report["cleanupPassed"], expr2=False)
            self.assertTrue(ca.is_symlink() if symlink else ca.exists())
            self.assertEqual(cleanup_commands(harness).calls, [])

    def test_trust_refresh_failure_preserves_failed_cleanup_outcome(self) -> None:
        """Keep trust refresh failure visible even after removing the owned CA file."""
        harness = self.harness()
        harness.commands = CleanupCommands(update_failure=True)
        ca = self.directory / "certificate"
        _ = ca.write_text("owned fixture certificate")
        harness.ca_path, harness.ca_digest = ca, sha256_file(ca)
        harness.cleanup()
        self.assertIs(harness.report["cleanupPassed"], expr2=False)
        self.assertEqual(harness.report["cleanupFailures"], ["CheckError"])


class HarnessEvidenceTests(HarnessTestCase):
    """Keep private original evidence intact while publishing only explicit summary fields."""

    def test_original_release_summary_includes_only_the_explicit_public_allowlist(self) -> None:
        """Retain full private outcomes without exposing database, environment, or backup data."""
        harness = self.harness()
        root = self.directory / "private-service"
        (root / "results").mkdir(parents=True)
        summary = {
            "action": "deploy",
            "revision": REVISION,
            "phase": "startup",
            "passed": False,
            "failure": "Candidate readiness failed",
            "rollbackAttempted": True,
            "rollbackPassed": True,
        }
        private_value = "PRIVATE_DATABASE_PASSWORD_AND_BACKUP_CONTENT"
        outcome = dict(
            summary,
            backupSha256="a" * 64,
            databaseDump=private_value,
            databasePassword=private_value,
            environment={"JWT_SECRET": private_value},
            arbitraryExtra=private_value,
        )

        def invoke(_args: list[str], **_kwargs: Unpack[CommandOptions]) -> TextResult:
            attempt = root / "results" / "release.owned"
            attempt.mkdir()
            _ = (attempt / "outcome.json").write_text(json.dumps(outcome))
            return TextResult(code=1)

        harness.commands = FakeCommands()
        fake_commands(harness).run.side_effect = invoke
        with patch.object(harness_module, "ROOT", root):
            attempt, actual = harness.invoke_release("deploy", {"revision": REVISION}, failure=True)
        self.assertEqual(actual, outcome, "Private original evidence must remain complete")
        self.assertEqual(
            read_object((attempt / "outcome.json").read_text(encoding="utf-8")), outcome
        )
        self.assertEqual(object_value(harness.report["lastRelease"]), summary)
        self.assertNotIn(private_value, json.dumps(harness.report))
        self.assertNotIn("backupSha256", object_value(harness.report["lastRelease"]))

    def test_release_status_requires_matching_original_boolean_and_exit_code(self) -> None:
        """Require boolean outcome agreement with the original release process status."""
        cases = [
            (False, 0, True, True),
            (True, 1, False, True),
            (True, 0, False, False),
            (True, 1, True, False),
            (False, 1, True, False),
            (False, 0, False, False),
            (True, 1, 0, False),
            (False, 0, 1, False),
        ]
        for index, (failure, code, passed, accepted) in enumerate(cases):
            with self.subTest(failure=failure, code=code, passed=passed, accepted=accepted):
                harness = self.harness()
                root = self.directory / f"service-{index}"
                (root / "results").mkdir(parents=True)
                outcome: JsonObject = {"action": "deploy", "revision": REVISION, "passed": passed}

                harness.commands = FakeCommands()
                fake_commands(harness).run.side_effect = outcome_command(self, root, outcome, code)
                with patch.object(harness_module, "ROOT", root):
                    if accepted:
                        _, actual = harness.invoke_release(
                            "deploy", {"revision": REVISION}, failure=failure
                        )
                        self.assertEqual(actual, outcome)
                    else:
                        with self.assertRaisesRegex(
                            harness_module.CheckError, "exit status and original outcome disagree"
                        ):
                            _ = harness.invoke_release(
                                "deploy", {"revision": REVISION}, failure=failure
                            )

    def test_failure_and_cleanup_failure_cannot_be_converted_into_a_pass(self) -> None:
        """Require both successful execution and cleanup before publishing a passing result."""
        for execution_passes, cleanup_passes in (
            (False, True),
            (True, False),
            (False, False),
            (True, True),
        ):
            with self.subTest(execution=execution_passes, cleanup=cleanup_passes):
                output = self.directory / f"main-{execution_passes}-{cleanup_passes}"
                harness = FakeLifecycle(execution_passes, cleanup_passes)
                retained_command: JsonObject | None = (
                    None
                    if execution_passes
                    else {
                        "number": 7,
                        "operation": "release-public.py deploy",
                        "exitStatus": 1,
                    }
                )
                harness.commands.last_failure = retained_command
                alarms: list[int] = []

                def alarm(seconds: int, *, pending: list[int] = alarms) -> int:
                    pending.append(seconds)
                    return 0

                with (
                    patch.object(harness_module, "Harness", return_value=harness),
                    patch.object(os, "umask"),
                    patch.object(signal, "signal"),
                    patch.object(signal, "alarm", alarm),
                    redirect_stdout(io.StringIO()),
                ):
                    status = harness_module.main(
                        [
                            "--disposable-host",
                            "--image",
                            "simplestchat-ci:production",
                            "--output",
                            str(output),
                        ]
                    )
                self.assertEqual(status, 0 if execution_passes and cleanup_passes else 1)
                report = read_object((output / "report.json").read_text(encoding="utf-8"))
                self.assertIs(report["passed"], execution_passes and cleanup_passes)
                self.assertEqual(report["lastNonzeroCommand"], retained_command)
                if not execution_passes:
                    self.assertEqual(report["failure"], "Original execution failure")
                self.assertEqual(harness.cleanup_calls, 1)
                self.assertEqual(alarms, [480, 100, 0])
                self.assertEqual(stat.S_IMODE((output / "report.json").stat().st_mode), 0o644)
                self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o755)
                self.assertEqual(stat.S_IMODE((output / "private").stat().st_mode), 0o700)


class HarnessValidatorDiagnosticTests(HarnessTestCase):
    """Expose bounded startup diagnostics only after independently checking validator identity."""

    def fixture(self) -> tuple[harness_module.Harness, Path, JsonObject]:
        """Return a verified fixed validator and typed command recorder."""
        harness = self.harness()
        harness.commands = FakeCommands()
        harness.commands.last_failure = {
            "number": 56,
            "operation": "release-public.py stage",
            "exitStatus": 1,
        }
        attempt = harness.private / "release-attempt"
        attempt.mkdir(mode=0o700)
        name = "scpub-release-validate-" + TOKEN
        _ = (attempt / "validation-name.txt").write_text(name)
        value: JsonObject = {
            "id": CONTAINER,
            "name": "/" + name,
            "image": IMAGE,
            "user": "10001:10001",
            "entrypoint": ["/usr/bin/timeout"],
            "network": "none",
            "readOnly": True,
            "state": {
                "ExitCode": 128,
                "Status": "created",
                "OOMKilled": False,
                "Error": "failed to create task: exec /usr/bin/timeout: no such file or directory",
            },
        }
        stderr = attempt / "validator.stderr"
        _ = stderr.write_bytes(b"")
        fake_commands(harness).docker.return_value = TextResult(json.dumps(value), error=stderr)
        return harness, attempt, value

    def test_verified_validator_startup_error_is_projected_without_general_logs_or_environment(
        self,
    ) -> None:
        """Publish only verified startup state and bounded stderr from the fixed validator."""
        harness, attempt, value = self.fixture()
        previous_failure = deepcopy(harness.commands.last_failure)
        harness.validator_startup_diagnostic(attempt, {"image": IMAGE})
        self.assertEqual(
            object_value(harness.report["validatorStartup"]),
            {
                "diagnosticUnavailable": False,
                "exitCode": 128,
                "status": "created",
                "oomKilled": False,
                "error": object_value(value["state"])["Error"],
                "errorTruncated": False,
                "runtimeLogs": {
                    "diagnosticUnavailable": False,
                    "stderr": "",
                    "stderrTruncated": False,
                },
            },
        )
        self.assertIs(harness.report["passed"], expr2=False)
        self.assertEqual(harness.commands.last_failure, previous_failure)
        calls = fake_commands(harness).docker.call_args_list
        self.assertEqual(len(calls), 2)
        call = calls[0]
        self.assertEqual(call.args[:2], ("inspect", "--format"))
        self.assertEqual(call.args[-1], "scpub-release-validate-" + TOKEN)
        self.assertEqual(call.kwargs, {"timeout": 10, "success": False})
        self.assertNotIn(".Config.Env", call.args[2])
        self.assertNotIn("logs", call.args)
        self.assertEqual(calls[1].args, ("logs", "--tail", "20", CONTAINER))
        self.assertEqual(calls[1].kwargs, {"timeout": 10, "success": False})
        self.assertNotIn(IMAGE, json.dumps(object_value(harness.report["validatorStartup"])))

    def test_missing_symlink_nonregular_oversized_or_invalid_name_never_contacts_docker(
        self,
    ) -> None:
        """Reject missing, replaced, oversized, and malformed validator name evidence locally."""
        for kind in ("missing", "symlink", "directory", "oversized", "invalid", "multiline"):
            with self.subTest(kind=kind):
                harness, attempt, _ = self.fixture()
                path = attempt / "validation-name.txt"
                path.unlink()
                if kind == "symlink":
                    target = attempt / "other"
                    _ = target.write_text("scpub-release-validate-" + TOKEN)
                    path.symlink_to(target)
                elif kind == "directory":
                    path.mkdir()
                elif kind == "oversized":
                    _ = path.write_text("x" * 129)
                elif kind == "invalid":
                    _ = path.write_text("unrelated-container")
                elif kind == "multiline":
                    _ = path.write_text("scpub-release-validate-" + TOKEN + "\nother")
                harness.validator_startup_diagnostic(attempt, {"image": IMAGE})
                self.assertEqual(
                    object_value(harness.report["validatorStartup"]),
                    {
                        "diagnosticUnavailable": True,
                        "reason": "ValidatorNameUnavailable",
                    },
                )
                fake_commands(harness).docker.assert_not_called()

    def test_every_identity_constraint_is_required_before_startup_error_is_published(self) -> None:
        """Withhold all container errors and logs until every identity constraint matches."""
        replacements: tuple[tuple[str, JsonValue], ...] = (
            ("id", "a" * 12),
            ("name", "/other"),
            ("image", FAILED_IMAGE),
            ("user", "0:0"),
            ("entrypoint", ["/bin/sh"]),
            ("network", "bridge"),
            ("readOnly", False),
        )
        for key, replacement in replacements:
            with self.subTest(key=key):
                harness, attempt, value = self.fixture()
                value[key] = replacement
                object_value(value["state"])["Error"] = "PRIVATE_ERROR_FROM_UNVERIFIED_CONTAINER"
                fake_commands(harness).docker.return_value = TextResult(json.dumps(value))
                harness.validator_startup_diagnostic(attempt, {"image": IMAGE})
                self.assertEqual(
                    object_value(harness.report["validatorStartup"]),
                    {
                        "diagnosticUnavailable": True,
                        "reason": "ValidatorOwnershipMismatch",
                    },
                )
                self.assertNotIn("PRIVATE_ERROR", json.dumps(harness.report))
                self.assertIs(harness.report["passed"], expr2=False)
                self.assertEqual(
                    fake_commands(harness).docker.call_count,
                    1,
                    "Unverified containers must never have logs read",
                )

    def test_verified_validator_stderr_is_bounded_and_checksum_stdout_remains_private(self) -> None:
        """Truncate verified stderr without publishing checksum stdout or changing failure."""
        harness, attempt, value = self.fixture()
        stderr = attempt / "validator-runtime.stderr"
        _ = stderr.write_bytes(b"x" * 5000)
        original_failure = deepcopy(harness.commands.last_failure)
        inspection = TextResult(json.dumps(value))
        logs = TextResult("PRIVATE_CHECKSUM_STDOUT", error=stderr)
        fake_commands(harness).docker.side_effect = [inspection, logs]
        harness.validator_startup_diagnostic(attempt, {"image": IMAGE})
        diagnostic = object_value(harness.report["validatorStartup"])
        self.assertEqual(
            diagnostic["runtimeLogs"],
            {
                "diagnosticUnavailable": False,
                "stderr": "x" * 4096,
                "stderrTruncated": True,
            },
        )
        self.assertEqual(diagnostic["exitCode"], 128)
        self.assertNotIn("PRIVATE_CHECKSUM_STDOUT", json.dumps(harness.report))
        self.assertEqual(harness.commands.last_failure, original_failure)
        self.assertIs(harness.report["passed"], expr2=False)

    def test_failed_log_retrieval_preserves_startup_state_and_original_release_failure(
        self,
    ) -> None:
        """Keep verified startup state when the secondary log request fails independently."""
        harness, attempt, value = self.fixture()
        stderr = attempt / "failed-log-command.stderr"
        _ = stderr.write_bytes(b"PRIVATE_DOCKER_COMMAND_ERROR")
        original_failure = deepcopy(harness.commands.last_failure)

        def inspect_or_logs(*args: str, **_kwargs: Unpack[CommandOptions]) -> TextResult:
            if args[0] == "inspect":
                return TextResult(json.dumps(value))
            self.assertEqual(args, ("logs", "--tail", "20", CONTAINER))
            harness.commands.last_failure = {
                "number": 58,
                "operation": "docker logs",
                "exitStatus": 1,
            }
            return TextResult("PRIVATE_DOCKER_COMMAND_STDOUT", code=1, error=stderr)

        fake_commands(harness).docker.side_effect = inspect_or_logs
        harness.validator_startup_diagnostic(attempt, {"image": IMAGE})
        diagnostic = object_value(harness.report["validatorStartup"])
        self.assertIs(diagnostic["diagnosticUnavailable"], expr2=False)
        self.assertEqual(diagnostic["error"], object_value(value["state"])["Error"])
        self.assertEqual(diagnostic["exitCode"], 128)
        self.assertEqual(diagnostic["runtimeLogs"], {"diagnosticUnavailable": True})
        self.assertEqual(harness.commands.last_failure, original_failure)
        self.assertNotIn("PRIVATE_DOCKER_COMMAND", json.dumps(harness.report))
        self.assertIs(harness.report["passed"], expr2=False)

    def test_symlinked_runtime_stderr_is_unavailable_without_losing_verified_state(self) -> None:
        """Refuse redirected stderr while retaining independently verified startup facts."""
        harness, attempt, value = self.fixture()
        source = attempt / "other.stderr"
        _ = source.write_bytes(b"PRIVATE_UNVERIFIED_FILE")
        link = attempt / "linked.stderr"
        link.symlink_to(source)
        fake_commands(harness).docker.side_effect = [
            TextResult(json.dumps(value)),
            TextResult(error=link),
        ]
        harness.validator_startup_diagnostic(attempt, {"image": IMAGE})
        diagnostic = object_value(harness.report["validatorStartup"])
        self.assertIs(diagnostic["diagnosticUnavailable"], expr2=False)
        self.assertEqual(diagnostic["runtimeLogs"], {"diagnosticUnavailable": True})
        self.assertNotIn("PRIVATE_UNVERIFIED_FILE", json.dumps(harness.report))

    def test_state_projection_is_typed_and_error_length_is_bounded(self) -> None:
        """Validate startup fields, exclude unknown data, and bound the published error text."""
        harness, attempt, value = self.fixture()
        object_value(value["state"])["Error"] = "x" * 4096
        object_value(value["state"])["Other"] = "PRIVATE_UNSELECTED_STATE"
        value["environment"] = ["SECRET=PRIVATE_UNSELECTED_ENVIRONMENT"]
        fake_commands(harness).docker.return_value = TextResult(json.dumps(value))
        harness.validator_startup_diagnostic(attempt, {"image": IMAGE})
        diagnostic = object_value(harness.report["validatorStartup"])
        self.assertEqual(diagnostic["error"], "x" * 2048)
        self.assertIs(diagnostic["errorTruncated"], expr2=True)
        self.assertNotIn("PRIVATE_UNSELECTED", json.dumps(diagnostic))
        replacements: tuple[tuple[str, JsonValue], ...] = (
            ("ExitCode", True),
            ("ExitCode", -1),
            ("OOMKilled", 0),
            ("Status", "PRIVATE_UNEXPECTED_STATUS"),
            ("Error", {"private": "data"}),
        )
        for key, replacement in replacements:
            with self.subTest(key=key, replacement=replacement):
                harness, attempt, value = self.fixture()
                object_value(value["state"])[key] = replacement
                fake_commands(harness).docker.return_value = TextResult(json.dumps(value))
                harness.validator_startup_diagnostic(attempt, {"image": IMAGE})
                self.assertEqual(
                    object_value(harness.report["validatorStartup"]),
                    {
                        "diagnosticUnavailable": True,
                        "reason": "ValidatorStateUnavailable",
                    },
                )

    def test_failed_or_malformed_inspection_is_unavailable_without_masking_original_command(
        self,
    ) -> None:
        """Retain the original release failure when inspection fails or returns malformed JSON."""
        for result in (TextResult("PRIVATE_DAEMON_ERROR", code=1), TextResult("malformed-json")):
            with self.subTest(code=result.code):
                harness, attempt, _ = self.fixture()
                previous_failure = deepcopy(harness.commands.last_failure)

                fake_commands(harness).docker.side_effect = failed_inspection(harness, result)
                harness.validator_startup_diagnostic(attempt, {"image": IMAGE})
                self.assertEqual(
                    object_value(harness.report["validatorStartup"]),
                    {
                        "diagnosticUnavailable": True,
                        "reason": "ValidatorInspectionUnavailable",
                    },
                )
                self.assertEqual(harness.commands.last_failure, previous_failure)
                self.assertNotIn("PRIVATE_DAEMON_ERROR", json.dumps(harness.report))

    def test_failed_release_retains_validator_evidence_before_the_existing_failure_assertion(
        self,
    ) -> None:
        """Capture startup diagnostics before enforcing the unchanged original release result."""
        harness, original_attempt, value = self.fixture()
        root = self.directory / "service"
        (root / "results").mkdir(parents=True)
        outcome = {
            "action": "stage",
            "revision": REVISION,
            "phase": "stage",
            "passed": False,
            "failure": "Command 004 failed; inspect private output",
        }

        def invoke(_args: list[str], **_kwargs: Unpack[CommandOptions]) -> TextResult:
            attempt = root / "results" / "release.owned"
            attempt.mkdir()
            _ = (attempt / "outcome.json").write_text(json.dumps(outcome))
            _ = (attempt / "validation-name.txt").write_bytes(
                (original_attempt / "validation-name.txt").read_bytes()
            )
            return TextResult(code=1)

        fake_commands(harness).run.side_effect = invoke
        with (
            patch.object(harness_module, "ROOT", root),
            self.assertRaisesRegex(
                harness_module.CheckError, "Release exit status and original outcome disagree"
            ),
        ):
            _ = harness.invoke_release("stage", {"revision": REVISION, "image": IMAGE})
        self.assertEqual(object_value(harness.report["lastRelease"]), outcome)
        self.assertEqual(
            object_value(harness.report["validatorStartup"])["error"],
            object_value(value["state"])["Error"],
        )
        self.assertIs(harness.report["passed"], expr2=False)
        self.assertEqual(
            read_object((root / "results/release.owned/outcome.json").read_text(encoding="utf-8")),
            outcome,
        )


class HarnessCommandTests(HarnessTestCase):
    """Retain original process outcomes privately and expose only safe operation summaries."""

    def test_nonzero_command_summary_excludes_private_arguments_and_environment(self) -> None:
        """Publish fixed operation names without private arguments or inherited environment."""
        private_value = "PRIVATE_DATABASE_PASSWORD_AND_BACKUP_CONTENT"
        cases = [
            (
                [
                    "/usr/bin/curl",
                    "--header",
                    "Authorization: Bearer " + private_value,
                    "https://localhost/ready",
                ],
                "curl",
            ),
            (
                [
                    *release.DOCKER,
                    "exec",
                    "--interactive",
                    CONTAINER,
                    "psql",
                    "--command",
                    "SELECT '" + private_value + "'",
                ],
                "docker exec",
            ),
            ([*release.DOCKER, "rm", private_value], "docker rm"),
            ([*release.DOCKER, "pull", private_value], "docker pull"),
            (
                [
                    *release.DOCKER,
                    "image",
                    "inspect",
                    "--format",
                    private_value,
                    "simplestchat-ci:production",
                ],
                "docker image inspect",
            ),
            (
                [
                    sys.executable,
                    "-B",
                    str(harness_module.FILES / "release-public.py"),
                    "deploy",
                    REVISION,
                ],
                "release-public.py deploy",
            ),
        ]
        for index, (arguments, operation) in enumerate(cases):
            with self.subTest(operation=operation):
                directory = self.directory / f"redacted-command-{index}"
                directory.mkdir(mode=0o700)
                commands = harness_module.Commands(directory)
                child = FakeChild()
                environment = dict(release.ENV, PRIVATE_FIXTURE_SECRET=private_value)
                with (
                    patch.object(subprocess, "Popen", return_value=child),
                    patch.object(release, "ENV", environment),
                ):
                    result = commands.run(arguments, success=False)
                self.assertEqual(result.code, 42)
                self.assertEqual(
                    commands.last_failure, {"number": 1, "operation": operation, "exitStatus": 42}
                )
                self.assertNotIn(private_value, json.dumps(commands.last_failure))
                self.assertNotIn("PRIVATE_FIXTURE_SECRET", json.dumps(commands.last_failure))
                self.assertNotIn(REVISION, json.dumps(commands.last_failure))
                self.assertNotIn(CONTAINER, json.dumps(commands.last_failure))
                private_evidence = read_object(
                    (directory / "command-001.json").read_text(encoding="utf-8")
                )
                self.assertEqual(private_evidence["command"], arguments)

    def test_cancelled_owned_process_marks_uncertainty_before_stopping_child(self) -> None:
        """Mark settlement uncertain before stopping an interrupted child process."""
        for interruption in (
            subprocess.TimeoutExpired(["owned-command"], 3),
            harness_module.CheckError("Owned command cancelled"),
        ):
            with self.subTest(interruption=type(interruption).__name__):
                directory = self.directory / type(interruption).__name__
                directory.mkdir(mode=0o700)
                commands = harness_module.Commands(directory)
                child = FakeChild(returncode=None, interruption=interruption)
                launch = FakeLaunch(child)
                stopped: list[object] = []

                def settle(
                    process: object,
                    *,
                    child: FakeChild = child,
                    commands: harness_module.Commands = commands,
                    stopped: list[object] = stopped,
                ) -> None:
                    self.assertIs(process, child)
                    self.assertIs(commands.settlement_unconfirmed, expr2=True)
                    stopped.append(process)
                    child.returncode = -15

                with (
                    patch.object(subprocess, "Popen", launch),
                    patch.object(release.Runner, "stop", settle),
                    self.assertRaises(type(interruption)),
                ):
                    _ = commands.run(["owned-command"], timeout=3)
                self.assertEqual(stopped, [child])
                self.assertIs(commands.settlement_unconfirmed, expr2=True)
                self.assertIs(launch.options["start_new_session"], expr2=True)
                self.assertEqual(launch.options["env"], release.ENV)
                report = read_object((directory / "command-001.json").read_text(encoding="utf-8"))
                self.assertEqual(report["exitStatus"], -15)
                self.assertEqual(report["timeoutSeconds"], 3)
                self.assertTrue((directory / "command-001.stdout").exists())
                self.assertTrue((directory / "command-001.stderr").exists())

    def test_completed_command_failure_is_not_confused_with_unsettled_execution(self) -> None:
        """A completed nonzero child needs no forced stop and creates no settlement ambiguity."""
        commands = harness_module.Commands(self.directory)
        child = FakeChild()
        with (
            patch.object(subprocess, "Popen", return_value=child),
            patch.object(release.Runner, "stop") as stop,
            self.assertRaisesRegex(harness_module.CheckError, "Command 001 failed"),
        ):
            _ = commands.run(["owned-command"])
        stop.assert_not_called()
        self.assertIs(commands.settlement_unconfirmed, expr2=False)
        self.assertEqual(
            read_object((self.directory / "command-001.json").read_text(encoding="utf-8"))[
                "exitStatus"
            ],
            42,
        )


class HarnessDerivativeTests(HarnessTestCase):
    """Build only deterministic fixture layers on the exact checked local production image."""

    def fixture(self, *, before: str = IMAGE, after: str = IMAGE) -> harness_module.Harness:
        """Pin the local base tag before and after a simulated derivative build."""
        harness = self.harness()
        harness.base_image = IMAGE
        harness.base_tag = f"simplestchat-release-test/base:{TOKEN}"
        harness.commands = FakeCommands()
        replies = iter([before, after])

        def docker(*args: str, **kwargs: Unpack[CommandOptions]) -> TextResult:
            if args[:2] == ("image", "inspect") and args[-1] == harness.base_tag:
                return TextResult(next(replies))
            if args[:2] == ("buildx", "build"):
                return TextResult()
            if args[:2] == ("image", "inspect") and args[-1].startswith(
                "simplestchat-release/production:"
            ):
                return TextResult(FAILED_IMAGE)
            msg = f"Unexpected fixture image operation: {args}, {kwargs}"
            raise AssertionError(msg)

        fake_commands(harness).docker.side_effect = docker
        return harness

    def test_derivative_uses_checked_local_tag_default_builder_and_no_production_rebuild(
        self,
    ) -> None:
        """Derive fixture labels locally without rebuilding production code or pulling a base."""
        harness = self.fixture()
        candidate = harness.derivative("candidate")
        directory = harness.private / "candidate"
        dockerfile = (directory / "Dockerfile").read_text(encoding="utf-8")
        self.assertTrue(dockerfile.startswith(f"FROM {harness.base_tag}\n"))
        self.assertNotIn("FROM sha256:", dockerfile)
        self.assertNotIn("RUN ", dockerfile)
        self.assertEqual(candidate["image"], FAILED_IMAGE)
        calls = fake_commands(harness).docker.call_args_list
        self.assertEqual(
            calls[0].args, ("image", "inspect", "--format", "{{.Id}}", harness.base_tag)
        )
        self.assertEqual(
            calls[1].args,
            (
                "buildx",
                "build",
                "--builder",
                "default",
                "--load",
                "--platform",
                "linux/amd64",
                "--provenance=false",
                "--sbom=false",
                "--network",
                "none",
                "--pull=false",
                "--tag",
                candidate["tag"],
                str(directory),
            ),
        )
        self.assertEqual(calls[1].kwargs, {"timeout": 90})
        self.assertEqual(calls[2].args, calls[0].args)
        self.assertEqual(
            calls[3].args, ("image", "inspect", "--format", "{{.Id}}", candidate["tag"])
        )
        self.assertEqual(len(calls), 4)

    def test_changed_base_tag_is_refused_before_build_or_candidate_acceptance(self) -> None:
        """Refuse a replaced base tag before building or accepting a derived fixture image."""
        for before, after, expected_builds, message in (
            (FAILED_IMAGE, IMAGE, 0, "before build"),
            (IMAGE, FAILED_IMAGE, 1, "during build"),
        ):
            with self.subTest(message=message):
                harness = self.fixture(before=before, after=after)
                with self.assertRaisesRegex(harness_module.CheckError, message):
                    _ = harness.derivative("candidate")
                calls = fake_commands(harness).docker.call_args_list
                self.assertEqual(
                    sum(call.args[:2] == ("buildx", "build") for call in calls), expected_builds
                )
                self.assertNotIn("candidate", harness.fixture_images)
                self.assertFalse(
                    any(
                        call.args[-1].startswith("simplestchat-release/production:")
                        for call in calls
                    )
                )

    def test_failure_fixture_is_a_deterministic_startup_exit_not_an_unbounded_workload(
        self,
    ) -> None:
        """Use a fixed immediate exit as the failure fixture without generating lasting load."""
        harness = self.fixture()
        _ = harness.derivative("failed")
        directory = harness.private / "failed"
        self.assertEqual(
            (directory / "failed-server").read_text(encoding="utf-8"),
            f"#!/bin/sh\necho '{harness_module.FAILURE_MARKER}' >&2\nexit 42\n",
        )
        self.assertIn(
            "COPY --chown=10001:10001 --chmod=0755 failed-server /app/simplestChat\n",
            (directory / "Dockerfile").read_text(encoding="utf-8"),
        )


if __name__ == "__main__":
    _ = unittest.main()
