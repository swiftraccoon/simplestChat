"""Offline release lifecycle tests; Docker, Compose and root ownership are mocked.

Only private temporary fixture files are changed. Archive validation, selection
render comparison, journal writes, readiness decisions and rollback are real.
"""

import hashlib
import io
import itertools
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
from collections.abc import Sequence
from contextlib import redirect_stdout
from copy import deepcopy
from dataclasses import dataclass, field
from pathlib import Path
from typing import IO, Protocol, Unpack, final, override
from unittest.mock import patch

# Fixture assertions and literal expected limits document the tested contract.
# ruff: noqa: S101, PLR2004
import test_support

# isort: split
import release_public as public
from release_artifact import ArtifactError, Manifest, sha256_file
from release_json import JsonObject, JsonValue, decode_json
from test_support import obj, string

ROOT = test_support.ROOT
FILES = ROOT / "ops/ansible/files"


def json_object(data: str | bytes) -> JsonObject:
    """Decode fixture evidence with the same checked JSON boundary as production."""
    return obj(decode_json(data))


REVISION = "a" * 40
OLD_REVISION = "b" * 40
NEW_IMAGE = "sha256:" + "c" * 64
OLD_IMAGE = "sha256:" + "d" * 64
TAG = f"simplestchat-release/production:{REVISION}"
MIGRATIONS = {"1": hashlib.sha384(b"SELECT 1;\n").hexdigest()}
IMAGE_TEMPLATE = (
    '{"id":{{json .Id}},"os":{{json .Os}},"architecture":{{json .Architecture}},'
    '"user":{{json .Config.User}},"labels":{{json .Config.Labels}},'
    '"cmd":{{json .Config.Cmd}},"entrypoint":{{json (index .Config "Entrypoint")}}}'
)


def fixture_manifest(digest: str) -> Manifest:
    """Return the exact validated manifest schema for the harmless fixture image."""
    return {
        "schemaVersion": 1,
        "revision": REVISION,
        "platform": "linux/amd64",
        "archiveSha256": digest,
        "imageTag": TAG,
        "migrations": dict(MIGRATIONS),
        "createdAt": "2026-09-13T12:00:00Z",
    }


def write_fixture_artifact(directory: Path) -> Manifest:
    """Write a private non-executable image archive and its matching manifest."""
    config = {
        "architecture": "amd64",
        "os": "linux",
        "config": {
            "User": "10001:10001",
            "Cmd": ["/app/simplestChat"],
            "Entrypoint": None,
            "Labels": {"org.opencontainers.image.revision": REVISION},
        },
    }
    with tarfile.open(directory / "image.tar", "w") as archive:
        files = {
            "manifest.json": json.dumps(
                [{"Config": "config.json", "RepoTags": [TAG], "Layers": ["layer.tar"]}]
            ).encode(),
            "config.json": json.dumps(config).encode(),
            "layer.tar": b"fixture-not-executed",
        }
        for name, data in files.items():
            member = tarfile.TarInfo(name)
            member.size = len(data)
            archive.addfile(member, io.BytesIO(data))
    manifest = fixture_manifest(sha256_file(directory / "image.tar"))
    _ = (directory / "release.json").write_text(json.dumps(manifest))
    return manifest


class FixtureRunner:
    """Model only the explicitly permitted Docker/Compose release commands."""

    def __init__(self, attempt: Path, config: Path) -> None:
        """Create in-memory service state without invoking external commands."""
        self.attempt: Path = attempt
        self.config: Path = config
        self.calls: list[tuple[str, tuple[str, ...], dict[str, object]]] = []
        self.app_image: str = OLD_IMAGE
        self.app_running: bool = True
        self.ups: int = 0
        self.database_healthy: bool = True
        self.database_migrations: dict[str, str] = dict(MIGRATIONS)
        self.image_migrations: dict[str, str] = dict(MIGRATIONS)
        self.preview_change: bool = False
        self.config_hash_mismatch: str | None = None
        self.candidate_wrong_image: bool = False
        self.candidate_unready: bool = False
        # Rooms reported by successive metrics polls; the last value repeats.
        # None models an unreadable endpoint.
        self.metrics_rooms: list[int] | None = [0]
        self.rollback_unready: bool = False
        self.validation_exit: bytes = b"0"
        self.validation_images: dict[str, str] = {}
        self.database: JsonObject = self._container("2", "sha256:" + "2" * 64, health=True)
        self.proxy: JsonObject = self._container("3", "sha256:" + "3" * 64)

    @staticmethod
    def _container(character: str, image: str, *, health: bool = False) -> JsonObject:
        value: JsonObject = {
            "id": character * 64,
            "image": image,
            "state": {
                "Running": True,
                "OOMKilled": False,
                "StartedAt": "2026-09-13T00:00:00Z",
            },
            "restarts": 0,
            "configHash": character * 64,
        }
        if health:
            obj(value, "state")["Health"] = {"Status": "healthy"}
        return value

    def factory(self, attempt: Path) -> "FixtureRunner":
        """Rebind command evidence to the transaction's newly created directory."""
        self.attempt = attempt
        return self

    def container(self, service: str) -> JsonObject:
        """Inspect only the named application or retained dependency fixture."""
        self.calls.append(("container", (service,), {}))
        if service == "simplestchat":
            if not self.app_running:
                message = "fixture application stopped"
                raise public.ReleaseError(message)
            return self._container("1", self.app_image)
        if service == "postgres":
            value = deepcopy(self.database)
            if not self.database_healthy:
                obj(value, "state", "Health")["Status"] = "unhealthy"
            return value
        if service == "caddy":
            return deepcopy(self.proxy)
        message = f"Unexpected service: {service}"
        raise AssertionError(message)

    def run(self, args: Sequence[str], **kwargs: Unpack[public.CommandOptions]) -> bytes:
        """Model only readiness requests and their selected failure conditions."""
        self.calls.append(("run", tuple(args), dict(kwargs)))
        if args[0] == "/usr/bin/curl" and args[-1].endswith("/metrics"):
            if self.metrics_rooms is None:
                message = "fixture metrics unavailable"
                raise public.ReleaseError(message)
            rooms = (
                self.metrics_rooms.pop(0) if len(self.metrics_rooms) > 1 else self.metrics_rooms[0]
            )
            return (
                f"simplestchat_connections_active 3\nsimplestchat_rooms_active {rooms}\n".encode()
            )
        if args[0] != "/usr/bin/curl" or not args[-1].endswith("/ready"):
            message = f"Unexpected external command: {args}"
            raise AssertionError(message)
        if (self.candidate_unready and self.ups == 1) or (self.rollback_unready and self.ups >= 2):
            message = "fixture readiness unavailable"
            raise public.ReleaseError(message)
        return b'{"status":"ready"}'

    def compose(  # noqa: C901, PLR0911 - explicit fixture command allowlist; no permissive fallback.
        self,
        *args: str,
        filename: Path | None = None,
        envfile: Path | None = None,
        **kwargs: Unpack[public.CommandOptions],
    ) -> bytes:
        """Render checked selection data or perform explicitly permitted app changes."""
        options: dict[str, object] = dict(kwargs)
        if filename is not None:
            options["filename"] = filename
        if envfile is not None:
            options["envfile"] = envfile
        self.calls.append(("compose", args, options))
        if args[:2] == ("config", "--hash"):
            service = args[-1]
            value = self.container(service)["configHash"]
            if service == self.config_hash_mismatch:
                value = "9" * 64
            return f"{service} {value}\n".encode()
        if "config" in args:
            selected_file = filename or self.config / "compose.public.yml"
            images = re.findall(r'image: "(sha256:[a-f0-9]{64})"', selected_file.read_text())
            assert len(images) == 2
            rendered: JsonObject = {
                "services": {
                    "simplestchat": {
                        "image": images[0],
                        "environment": {
                            "RUN_MIGRATIONS": "false",
                            "WEBAUTHN_ORIGIN": "https://fixture.invalid",
                        },
                    },
                    "migrate": {"image": images[1]},
                    "postgres": {"image": self.database["image"]},
                    "caddy": {"image": self.proxy["image"]},
                }
            }
            selected_environment = self.config / "app.env"
            candidate_environment = re.search(
                r'- "([^"]+/candidate.env)"', selected_file.read_text()
            )
            if candidate_environment:
                selected_environment = Path(candidate_environment.group(1))
            obj(rendered, "services", "simplestchat", "environment").update(
                dict(
                    line.split("=", 1)
                    for line in selected_environment.read_text().splitlines()
                    if "=" in line
                )
            )
            if filename and self.preview_change:
                obj(rendered, "services", "simplestchat", "environment")["ALLOW_AD_HOC_ROOMS"] = (
                    "true"
                )
            return json.dumps(rendered).encode()
        if "ps" in args and args[-1] == "migrate":
            return b""
        if args == ("ps", "--all", "--quiet", "simplestchat"):
            return ("1" * 64).encode()
        if args[0] == "stop":
            assert args == ("stop", "--timeout", "30", "simplestchat")
            self.app_running = False
            return b""
        if args[0] == "up":
            assert args == (
                "up",
                "--detach",
                "--no-build",
                "--pull",
                "never",
                "--no-deps",
                "simplestchat",
            )
            self.ups += 1
            selected = string(json_object((self.config / "images.json").read_text()), "serverImage")
            self.app_image = OLD_IMAGE if self.candidate_wrong_image and self.ups == 1 else selected
            self.app_running = True
            return b""
        if args[0] == "logs":
            return b"fixture candidate failure retained\n"
        message = f"Unexpected Compose command: {args}"
        raise AssertionError(message)

    def docker(self, *args: str, **kwargs: Unpack[public.CommandOptions]) -> bytes:  # noqa: C901, PLR0911, PLR0912 - explicit fake command allowlist matches each release phase.
        """Model isolated validation and read-only backup operations, never migrations."""
        self.calls.append(("docker", args, dict(kwargs)))
        if args[0] == "inspect" and args[-1] == "1" * 64:
            assert args[1:3] == ("--format", '{"image":{{json .Image}},"state":{{json .State}}}')
            return json.dumps(
                {
                    "image": self.app_image,
                    "state": {
                        "Running": self.app_running,
                        "OOMKilled": False,
                        "ExitCode": 0,
                    },
                }
            ).encode()
        if args[:2] == ("image", "load"):
            return b"Loaded fixture image\n"
        if args[:2] == ("image", "inspect"):
            selector = args[-1]
            assert selector in (TAG, NEW_IMAGE, OLD_IMAGE)
            image = OLD_IMAGE if selector == OLD_IMAGE else NEW_IMAGE
            return json.dumps(
                {
                    "id": image,
                    "os": "linux",
                    "architecture": "amd64",
                    "user": "10001:10001",
                    "labels": {
                        "org.opencontainers.image.revision": OLD_REVISION
                        if image == OLD_IMAGE
                        else REVISION
                    },
                    "cmd": ["/app/simplestChat"],
                    "entrypoint": None,
                }
            ).encode()
        if args[0] == "create":
            assert args[args.index("--network") + 1] == "none"
            assert args[args.index("--entrypoint") + 1] == "/usr/bin/timeout"
            assert "--kill-after=2s" in args
            assert "10s" in args
            assert "/bin/sh" in args
            container = "4" * 64
            self.validation_images[container] = args[args.index("--entrypoint") + 2]
            return container.encode()
        if args[0] in ("start", "wait", "logs", "rm", "inspect", "stop", "kill"):
            container = args[-1]
            assert container in self.validation_images, f"Unknown validation container: {args}"
            if args[0] == "wait":
                return self.validation_exit
            if args[0] == "logs":
                return "".join(
                    f"{checksum}  /app/migrations/{int(version):03d}_fixture.sql\n"
                    for version, checksum in self.image_migrations.items()
                ).encode()
            if args[0] == "inspect":
                return json.dumps(
                    {
                        "id": container,
                        "image": self.validation_images[container],
                        "state": {"Running": False, "OOMKilled": False, "ExitCode": 0},
                    }
                ).encode()
            return b""
        if args[0] == "exec":
            if "psql" in args:
                return "".join(
                    f"{version} t {checksum}\n"
                    for version, checksum in self.database_migrations.items()
                ).encode()
            if "pg_dump" in args:
                data = b"PGDMP-fixture-only"
                output_path = kwargs.get("output_path")
                if output_path is not None:
                    _ = output_path.write_bytes(data)
                    return b""
                return data
            if "pg_restore" in args:
                input_path = kwargs.get("input_path")
                supplied = (
                    input_path.read_bytes() if input_path is not None else kwargs.get("input_data")
                )
                assert supplied == b"PGDMP-fixture-only"
                return b"fixture validated archive table of contents\n"
        message = f"Unexpected Docker command: {args}"
        raise AssertionError(message)


@final
class ImageRunner(FixtureRunner):
    """Return an explicitly supplied image inspection while recording exact argv."""

    def __init__(self, payload: bytes) -> None:
        """Store harmless response bytes without opening the placeholder paths."""
        super().__init__(Path(), Path())
        self.payload = payload

    @override
    def docker(self, *args: str, **kwargs: Unpack[public.CommandOptions]) -> bytes:
        """Return the selected image metadata and retain its exact inspection query."""
        self.calls.append(("docker", args, dict(kwargs)))
        return self.payload


@final
class PublicImageIdentityTests(unittest.TestCase):
    """Keep optional image fields compatible without weakening mandatory identity checks."""

    def metadata(self) -> JsonObject:
        """Create a valid runtime image identity for one reviewed revision."""
        return {
            "id": NEW_IMAGE,
            "os": "linux",
            "architecture": "amd64",
            "user": "10001:10001",
            "labels": {"org.opencontainers.image.revision": REVISION},
            "cmd": ["/app/simplestChat"],
            "entrypoint": None,
        }

    def test_only_optional_entrypoint_uses_safe_map_lookup(self) -> None:
        """Only optional entrypoint uses safe map lookup."""
        runner = ImageRunner(json.dumps(self.metadata()).encode())
        self.assertEqual(public.image_identity(runner, TAG, REVISION), NEW_IMAGE)
        self.assertEqual(
            runner.calls, [("docker", ("image", "inspect", "--format", IMAGE_TEMPLATE, TAG), {})]
        )
        self.assertEqual(IMAGE_TEMPLATE.count("index "), 1)
        self.assertNotIn(".Config.Entrypoint", IMAGE_TEMPLATE)

    def test_empty_entrypoint_forms_are_allowed_but_nonempty_or_malformed_values_are_rejected(
        self,
    ) -> None:
        """Empty entrypoint forms are allowed but nonempty or malformed values are rejected."""
        entrypoints: tuple[JsonValue, ...] = (
            None,
            [],
            ["/bin/sh"],
            ["/app/simplestChat"],
            "",
            False,
            {},
        )
        for entrypoint in entrypoints:
            with self.subTest(entrypoint=entrypoint):
                value = self.metadata()
                value["entrypoint"] = entrypoint
                runner = ImageRunner(json.dumps(value).encode())
                if entrypoint is None or entrypoint == []:
                    self.assertEqual(public.image_identity(runner, TAG, REVISION), NEW_IMAGE)
                else:
                    with self.assertRaisesRegex(public.ReleaseError, "Unexpected image entrypoint"):
                        _ = public.image_identity(runner, TAG, REVISION)

    def test_required_runtime_identity_and_revision_remain_strict(self) -> None:
        """Required runtime identity and revision remain strict."""
        replacements: tuple[tuple[str, JsonValue, str], ...] = (
            ("id", "c" * 64, "content-addressed"),
            ("os", "windows", "platform or runtime user"),
            ("architecture", "arm64", "platform or runtime user"),
            ("user", "0:0", "platform or runtime user"),
            ("labels", {}, "revision mismatch"),
            ("labels", {"org.opencontainers.image.revision": OLD_REVISION}, "revision mismatch"),
            ("cmd", [], "entrypoint"),
            ("cmd", ["/bin/sh"], "entrypoint"),
        )
        for key, replacement, reason in replacements:
            with self.subTest(key=key, replacement=replacement):
                value = self.metadata()
                value[key] = replacement
                runner = ImageRunner(json.dumps(value).encode())
                with self.assertRaisesRegex(public.ReleaseError, reason):
                    _ = public.image_identity(runner, TAG, REVISION)

    @unittest.skipUnless(
        shutil.which("go"), "Optional Go template check requires local Go; no Docker daemon is used"
    )
    def test_actual_go_template_tolerates_only_the_optional_absent_entrypoint(self) -> None:
        """Actual go template tolerates only the optional absent entrypoint."""
        source = r"""package main
import ("encoding/json"; "fmt"; "os"; "text/template")
func main() {
    var input struct { Format string; Value any }
    if err := json.NewDecoder(os.Stdin).Decode(&input); err != nil { panic(err) }
    functions := template.FuncMap{"json": func(value any) (string, error) {
        result, err := json.Marshal(value); return string(result), err
    }}
    configured := template.New("inspect").Option("missingkey=error").Funcs(functions)
    rendered, err := configured.Parse(input.Format)
    if err == nil { err = rendered.Execute(os.Stdout, input.Value) }
    if err != nil { fmt.Fprintln(os.Stderr, err); os.Exit(1) }
}
"""
        with tempfile.TemporaryDirectory(prefix="simplestchat-image-template.") as temporary:
            directory = Path(temporary)
            code, executable = directory / "inspect.go", directory / "inspect"
            _ = code.write_text(source)
            environment = dict(
                os.environ,
                GOTOOLCHAIN="local",
                GOPROXY="off",
                GOSUMDB="off",
                GOWORK="off",
                CGO_ENABLED="0",
            )
            go = shutil.which("go")
            assert go is not None
            build = subprocess.run(  # noqa: S603 - fixed source fixture, local Go, and network-disabled environment.
                [go, "build", "-o", str(executable), str(code)],
                env=environment,
                capture_output=True,
                text=True,
                timeout=60,
                check=False,
            )
            self.assertEqual(build.returncode, 0, build.stderr)
            captured = ImageRunner(json.dumps(self.metadata()).encode())
            _ = public.image_identity(captured, TAG, REVISION)
            actual_template = captured.calls[0][1][3]
            value: JsonObject = {
                "Id": NEW_IMAGE,
                "Os": "linux",
                "Architecture": "amd64",
                "Config": {
                    "User": "10001:10001",
                    "Labels": {"org.opencontainers.image.revision": REVISION},
                    "Cmd": ["/app/simplestChat"],
                },
            }

            def render(
                data: JsonObject, template: str = actual_template
            ) -> subprocess.CompletedProcess[str]:
                return subprocess.run(  # noqa: S603 - execute only this test's locally compiled fixture.
                    [str(executable)],
                    input=json.dumps({"Format": template, "Value": data}),
                    capture_output=True,
                    text=True,
                    timeout=5,
                    check=False,
                )

            forms: tuple[tuple[str, JsonValue], ...] = (
                ("omitted", None),
                ("null", None),
                ("empty", []),
                ("nonempty", ["/bin/sh"]),
            )
            for form, entrypoint in forms:
                with self.subTest(form=form):
                    data = deepcopy(value)
                    if form != "omitted":
                        obj(data, "Config")["Entrypoint"] = entrypoint
                    result = render(data)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(json_object(result.stdout)["entrypoint"], entrypoint)
                    runner = ImageRunner(result.stdout.encode())
                    if form == "nonempty":
                        with self.assertRaisesRegex(
                            public.ReleaseError, "Unexpected image entrypoint"
                        ):
                            _ = public.image_identity(runner, TAG, REVISION)
                    else:
                        self.assertEqual(public.image_identity(runner, TAG, REVISION), NEW_IMAGE)
            # Demonstrate the original Docker failure and retain strict Go
            # lookup behavior for every mandatory Config identity field.
            broken = actual_template.replace('(index .Config "Entrypoint")', ".Config.Entrypoint")
            self.assertNotEqual(render(value, broken).returncode, 0)
            for required in ("User", "Labels", "Cmd"):
                data = deepcopy(value)
                del obj(data, "Config")[required]
                with self.subTest(missing=required):
                    self.assertNotEqual(render(data).returncode, 0)


class PatchHandle(Protocol):
    """Describe reversible test substitutions without dynamically typed results."""

    def start(self) -> object:
        """Activate the selected substitution."""
        ...

    def stop(self) -> object:
        """Restore the original attribute."""
        ...


@final
class PublicReleaseTests(unittest.TestCase):
    """Prove image-only releases preserve dependencies and failed rollback evidence."""

    def __init__(self, methodName: str = "runTest") -> None:  # noqa: N803 - unittest's public constructor keyword.
        """Initialize harmless placeholders without creating resources during discovery."""
        super().__init__(methodName)
        self.temporary: tempfile.TemporaryDirectory[str] | None = None
        self.root = Path()
        self.config = Path()
        self.work = Path()
        self.directory = Path()
        self.manifest = fixture_manifest("0" * 64)
        self.before: dict[str, bytes] = {}
        self.attempt = Path()
        self.runner = FixtureRunner(self.attempt, self.config)
        self.patches: list[PatchHandle] = []
        self.previous_umask = 0

    @override
    def setUp(self) -> None:
        """Create isolated fixtures and register reversible test substitutions."""
        self.temporary = tempfile.TemporaryDirectory(prefix="simplestchat-public-release-test.")
        self.root = Path(self.temporary.name).resolve()
        self.config = self.root / "config"
        self.work = self.root / "work"
        for directory in (self.config, self.work, self.root / "results", self.root / "releases"):
            directory.mkdir(mode=0o700)
        (self.work / "workload.lock").touch(mode=0o600)
        self.directory = self.root / "releases" / REVISION
        self.directory.mkdir(mode=0o700)
        self.manifest = write_fixture_artifact(self.directory)
        _ = (self.config / "images.json").write_text(
            json.dumps({"revision": OLD_REVISION, "serverImage": OLD_IMAGE})
        )
        compose = (
            f'services:\n  simplestchat:\n    image: "{OLD_IMAGE}"\n'
            "    env_file:\n      - ./app.env\n"
            f'  migrate:\n    image: "{OLD_IMAGE}"\n'
        )
        _ = (self.config / "compose.public.yml").write_text(compose)
        _ = (self.config / "app.env").write_text(
            f"SIMPLESTCHAT_IMAGE={OLD_IMAGE}\nRUN_MIGRATIONS=false\n"
            + "METRICS_TOKEN=fixture-metrics-token-with-at-least-32-bytes\n"
        )
        self.before = {name: (self.config / name).read_bytes() for name in public.SELECTION}
        self.attempt = self.root / "direct-attempt"
        self.attempt.mkdir(mode=0o700)
        self.runner = FixtureRunner(self.attempt, self.config)
        self.patches = [
            patch.object(public, "ROOT", self.root),
            patch.object(public, "CONFIG", self.config),
            patch.object(public, "WORK", self.work),
            patch.object(public, "protected"),
            patch.object(public, "Runner", self.runner.factory),
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

    def stage(self) -> JsonObject:
        """Stage the fixture using the real artifact and image validation transaction."""
        return public.stage(self.runner, self.directory, self.manifest)

    def execute(self, action: str = "deploy") -> None:
        """Run one real release CLI action while retaining its private fixture evidence."""
        with (
            patch.object(sys, "argv", ["release-public.py", action, REVISION]),
            redirect_stdout(io.StringIO()),
        ):
            public.main()

    def report(self) -> JsonObject:
        """Read the outcome from the single expected release attempt."""
        files = list((self.root / "results").glob("release.*/outcome.json"))
        self.assertEqual(len(files), 1)
        return json_object(files[0].read_text())

    def app_mutations(self) -> list[tuple[str, ...]]:
        """Return only application stop and replacement calls affecting availability."""
        return [
            args
            for kind, args, _ in self.runner.calls
            if kind == "compose" and args[0] in ("stop", "up")
        ]

    def assert_config_unchanged(self) -> None:
        """Require every installed selection file to retain its exact original bytes."""
        self.assertEqual(
            {name: (self.config / name).read_bytes() for name in public.SELECTION}, self.before
        )

    def test_validator_uses_init_and_bounded_logs_without_weakening_isolation(self) -> None:
        """Validator uses init and bounded logs without weakening isolation."""
        self.assertEqual(public.packaged_migrations(self.runner, NEW_IMAGE), MIGRATIONS)
        creates = [
            (args, kwargs)
            for kind, args, kwargs in self.runner.calls
            if kind == "docker" and args[0] == "create"
        ]
        self.assertEqual(len(creates), 1)
        args, kwargs = creates[0]
        self.assertRegex(args[2], r"^scpub-release-validate-[a-f0-9]{32}$")
        self.assertEqual(
            args,
            (
                "create",
                "--name",
                args[2],
                "--init",
                "--network",
                "none",
                "--pull",
                "never",
                "--read-only",
                "--user",
                "10001:10001",
                "--cap-drop",
                "ALL",
                "--security-opt",
                "no-new-privileges",
                "--memory",
                "128m",
                "--cpus",
                "0.5",
                "--pids-limit",
                "32",
                "--log-driver",
                "local",
                "--log-opt",
                "max-size=1m",
                "--log-opt",
                "max-file=1",
                "--log-opt",
                "compress=false",
                "--entrypoint",
                "/usr/bin/timeout",
                NEW_IMAGE,
                "--signal=TERM",
                "--kill-after=2s",
                "10s",
                "/bin/sh",
                "-c",
                'for file in /app/migrations/*.sql; do sha384sum "$file" || exit; done',
            ),
        )
        self.assertEqual(kwargs, {})
        self.assertTrue(json_object((self.root / "release-state.json").read_text())["finalized"])
        self.assertEqual(self.app_mutations(), [])
        self.assert_config_unchanged()

    def test_stage_validates_image_and_migrations_then_reuses_target_local_identity(self) -> None:
        """Stage validates image and migrations then reuses target local identity."""
        staged = self.stage()
        self.assertEqual(staged["serverImage"], NEW_IMAGE)
        self.assertEqual(staged["manifestSha256"], sha256_file(self.directory / "release.json"))
        self.assertEqual(
            sum(
                args[:2] == ("image", "load")
                for kind, args, _ in self.runner.calls
                if kind == "docker"
            ),
            1,
        )
        self.runner.calls.clear()
        self.assertEqual(self.stage(), staged)
        self.assertFalse(
            any(
                args[:2] == ("image", "load") or args[0] == "create"
                for kind, args, _ in self.runner.calls
                if kind == "docker"
            )
        )
        self.assertEqual(self.app_mutations(), [])
        self.assert_config_unchanged()

    def test_archive_or_staged_manifest_tampering_fails_before_loading(self) -> None:
        """Archive or staged manifest tampering fails before loading."""
        _ = self.stage()
        self.runner.calls.clear()
        with (self.directory / "image.tar").open("ab") as output:
            _ = output.write(b"changed")
        with self.assertRaises(ArtifactError):
            _ = self.stage()
        self.assertEqual(self.runner.calls, [])
        self.manifest = write_fixture_artifact(self.directory)
        record = json_object((self.directory / "staged.json").read_text())
        record["manifestSha256"] = "0" * 64
        _ = (self.directory / "staged.json").write_text(json.dumps(record))
        with self.assertRaisesRegex(public.ReleaseError, "Staged release changed"):
            _ = self.stage()
        self.assertEqual(self.runner.calls, [])

    def test_stage_checksum_mismatch_does_not_publish_success_or_touch_public_app(self) -> None:
        """Stage checksum mismatch does not publish success or touch public app."""
        self.runner.image_migrations = {"1": "f" * 96}
        with self.assertRaisesRegex(public.ReleaseError, "migration manifest"):
            self.execute("stage")
        self.assertFalse((self.directory / "staged.json").exists())
        self.assertFalse(self.report()["passed"])
        self.assertEqual(self.app_mutations(), [])
        self.assert_config_unchanged()

    def test_schema_change_is_rejected_before_stop_without_rollback(self) -> None:
        """Schema change is rejected before stop without rollback."""
        _ = self.stage()
        journal = (self.root / "release-state.json").read_bytes()
        self.runner.database_migrations = {"1": "e" * 96}
        with self.assertRaisesRegex(public.ReleaseError, "Schema changes"):
            self.execute()
        self.assertFalse(self.report()["passed"])
        self.assertNotIn("rollbackAttempted", self.report())
        self.assertEqual(self.app_mutations(), [])
        self.assertEqual((self.root / "release-state.json").read_bytes(), journal)
        self.assert_config_unchanged()

    def test_success_replaces_only_app_and_preserves_database_and_proxy(self) -> None:
        """Success replaces only app and preserves database and proxy."""
        _ = self.stage()
        database, proxy = deepcopy(self.runner.database), deepcopy(self.runner.proxy)
        self.execute()
        report = self.report()
        self.assertTrue(report["passed"])
        self.assertEqual(report["phase"], "complete")
        self.assertNotIn("rollbackAttempted", report)
        self.assertEqual(self.runner.app_image, NEW_IMAGE)
        self.assertEqual(self.runner.database, database)
        self.assertEqual(self.runner.proxy, proxy)
        self.assertEqual(len(self.app_mutations()), 2)
        self.assertTrue(all(args[-1] == "simplestchat" for args in self.app_mutations()))
        self.assertEqual(
            json_object((self.config / "images.json").read_text())["serverImage"], NEW_IMAGE
        )
        self.assertIn(f"SIMPLESTCHAT_IMAGE={NEW_IMAGE}\n", (self.config / "app.env").read_text())
        self.assertTrue(json_object((self.root / "release-state.json").read_text())["finalized"])
        self.assertIn("backupSha256", report)
        for kind, args, _ in self.runner.calls:
            if kind == "docker":
                self.assertNotIn("pull", args)
                self.assertNotIn("build", args)
                if "pg_restore" in args:
                    self.assertIn("--list", args, "A release must never restore the live database")

    def test_wrong_candidate_image_rolls_back_once_but_remains_failed(self) -> None:
        """Wrong candidate image rolls back once but remains failed."""
        _ = self.stage()
        self.runner.candidate_wrong_image = True
        with self.assertRaisesRegex(public.ReleaseError, "not the staged image"):
            self.execute()
        self.assert_failed_rollback()

    def test_candidate_readiness_failure_rolls_back_once_but_remains_failed(self) -> None:
        """Candidate readiness failure rolls back once but remains failed."""
        _ = self.stage()
        self.runner.candidate_unready = True
        with self.assertRaisesRegex(public.ReleaseError, "readiness deadline"):
            self.execute()
        self.assert_failed_rollback()

    def test_turn_activation_reuses_the_image_and_changes_only_its_three_settings(self) -> None:
        """Explicit relay activation keeps the app image, proxy and database identities."""
        turn = public.TurnConfiguration(domain="fixture.invalid", secret="a" * 64)
        manifest = deepcopy(self.manifest)
        manifest["revision"] = OLD_REVISION
        report: JsonObject = {}
        public.deploy(self.runner, manifest, {"serverImage": OLD_IMAGE}, report, turn=turn)
        self.assertEqual(self.runner.app_image, OLD_IMAGE)
        self.assertEqual(self.runner.ups, 1)
        self.assertEqual(
            (self.config / "compose.public.yml").read_bytes(), self.before["compose.public.yml"]
        )
        environment = dict(
            line.split("=", 1) for line in (self.config / "app.env").read_text().splitlines()
        )
        for key, value in turn.environment().items():
            self.assertEqual(environment.pop(key), value)
        self.assertEqual(
            environment,
            dict(line.split("=", 1) for line in self.before["app.env"].decode().splitlines()),
        )
        self.assertEqual(report["phase"], "complete")
        self.assertIn("backupSha256", report)
        self.assertTrue(all(args[-1] == "simplestchat" for args in self.app_mutations()))

    def test_turn_activation_failure_restores_the_prior_environment_with_the_same_image(
        self,
    ) -> None:
        """Readiness failure removes advertised relay settings and preserves original failure."""
        self.runner.candidate_unready = True
        manifest = deepcopy(self.manifest)
        manifest["revision"] = OLD_REVISION
        report: JsonObject = {}
        with self.assertRaisesRegex(public.ReleaseError, "readiness deadline"):
            public.deploy(
                self.runner,
                manifest,
                {"serverImage": OLD_IMAGE},
                report,
                turn=public.TurnConfiguration(domain="fixture.invalid", secret="a" * 64),
            )
        self.assertTrue(report["rollbackPassed"])
        self.assertEqual(self.runner.app_image, OLD_IMAGE)
        self.assertEqual(self.runner.ups, 2)
        self.assert_config_unchanged()

    def test_turn_and_image_selection_can_share_one_reviewed_replacement(self) -> None:
        """A staged app update and first relay activation need only one interruption."""
        staged = self.stage()
        report: JsonObject = {}
        turn = public.TurnConfiguration(domain="fixture.invalid", secret="a" * 64)
        public.deploy(self.runner, self.manifest, staged, report, turn=turn)
        self.assertEqual(self.runner.ups, 1)
        self.assertEqual(self.runner.app_image, NEW_IMAGE)
        self.assertEqual(report["phase"], "complete")
        environment = dict(
            line.split("=", 1) for line in (self.config / "app.env").read_text().splitlines()
        )
        self.assertEqual(environment["SIMPLESTCHAT_IMAGE"], NEW_IMAGE)
        for key, value in turn.environment().items():
            self.assertEqual(environment[key], value)
        self.assertTrue(all(args[-1] == "simplestchat" for args in self.app_mutations()))

    def test_turn_activation_refuses_existing_settings_and_unrelated_preview_changes(self) -> None:
        """Rotation or hidden runtime changes must never enter the replacement phase."""
        turn = public.TurnConfiguration(domain="fixture.invalid", secret="a" * 64)
        old: JsonObject = {"serverImage": OLD_IMAGE}
        for existing in ("TURN_SECRET=old\n", "TURN_URLS=turn:other.invalid\n", "TURN_TTL=600\n"):
            with self.subTest(existing=existing):
                _ = (self.config / "app.env").write_bytes(
                    self.before["app.env"] + existing.encode()
                )
                with self.assertRaisesRegex(public.ReleaseError, "already configured"):
                    _ = public.candidate_selection(self.runner, OLD_IMAGE, old, turn)
        _ = (self.config / "app.env").write_bytes(self.before["app.env"])
        self.runner.preview_change = True
        with self.assertRaisesRegex(public.ReleaseError, "beyond the reviewed selection"):
            _ = public.candidate_selection(self.runner, OLD_IMAGE, old, turn)
        self.assertEqual(self.app_mutations(), [])

    def test_turn_values_cannot_inject_urls_or_environment_and_secret_repr_is_private(self) -> None:
        """Only the fixed managed URLs and an opaque 256-bit secret can be advertised."""
        for domain, secret in (
            ("chat.example/evil", "a" * 64),
            ("chat.example\nBAD=1", "a" * 64),
            ("chat.example", "a" * 63 + "\n"),
        ):
            with (
                self.subTest(domain=domain),
                self.assertRaisesRegex(public.ReleaseError, "Invalid managed"),
            ):
                _ = public.TurnConfiguration(domain=domain, secret=secret).environment()
        self.assertNotIn(
            "a" * 64, repr(public.TurnConfiguration(domain="chat.example", secret="a" * 64))
        )

    def test_malformed_readiness_shapes_are_retried_only_until_the_deadline(self) -> None:
        """A non-object or ambiguous readiness response cannot pass or escape the deadline."""
        for response in (b"[]", b"null", b'{"status":"ready","status":"ready"}'):
            with (
                self.subTest(response=response),
                patch.object(self.runner, "run", return_value=response),
                self.assertRaisesRegex(public.ReleaseError, "readiness deadline exceeded"),
            ):
                public.ready(self.runner, seconds=0)

    def test_replacement_waits_for_rooms_to_empty_and_reports_it(self) -> None:
        """With active rooms the app is replaced only after they empty, within the bound."""
        _ = self.stage()
        self.runner.metrics_rooms = [2, 1, 0]
        # The harness clock jumps 100 s per call; polling needs a slow one.
        with (
            patch.object(time, "sleep") as sleep,
            patch.object(time, "monotonic", side_effect=itertools.count(0.0, 0.5)),
        ):
            self.execute_quiet(30)
        report = self.report()
        self.assertTrue(report["passed"])
        self.assertEqual(report["roomsActiveAtReplacement"], 0)
        self.assertEqual(report["quietWaitRequestedSeconds"], 30)
        self.assertEqual(sleep.call_count, 2)
        polls = [
            call
            for call in self.runner.calls
            if call[0] == "run" and call[1][-1].endswith("/metrics")
        ]
        self.assertEqual(len(polls), 3)
        stop = next(
            index
            for index, call in enumerate(self.runner.calls)
            if call[0] == "compose" and call[1][0] == "stop"
        )
        last_poll = max(
            index
            for index, call in enumerate(self.runner.calls)
            if call[0] == "run" and call[1][-1].endswith("/metrics")
        )
        self.assertLess(last_poll, stop, "polling finishes before the app is stopped")
        self.assertEqual(self.runner.app_image, NEW_IMAGE)
        self.assertTrue(
            all("Bearer" not in " ".join(call[1]) for call in self.runner.calls),
            "the token never appears in a command line",
        )

    def test_replacement_proceeds_at_the_quiet_deadline_and_reports_active_rooms(self) -> None:
        """Rooms still active at the deadline are recorded, not treated as a failure."""
        _ = self.stage()
        self.runner.metrics_rooms = [3]
        clock = iter([0.0, 0.0, 0.0, 1.0, 1.5, 3.0, 3.5, 100.0, 100.0, 100.0, 100.0, 100.0])
        with (
            patch.object(time, "sleep"),
            patch.object(time, "monotonic", side_effect=lambda: next(clock, 200.0)),
        ):
            self.execute_quiet(2)
        report = self.report()
        self.assertTrue(report["passed"])
        self.assertEqual(report["roomsActiveAtReplacement"], 3)
        self.assertEqual(self.runner.app_image, NEW_IMAGE)

    def test_replacement_proceeds_when_metrics_are_unavailable_or_no_token_is_set(self) -> None:
        """Missing observability never blocks a release; the report says it could not tell."""
        _ = self.stage()
        self.runner.metrics_rooms = None
        with patch.object(time, "sleep") as sleep:
            self.execute_quiet(30)
        report = self.report()
        self.assertTrue(report["passed"])
        self.assertIsNone(report["roomsActiveAtReplacement"])
        self.assertEqual(sleep.call_count, 0)
        self.assertEqual(self.runner.app_image, NEW_IMAGE)

    def test_quiet_wait_without_a_metrics_token_is_skipped(self) -> None:
        """An environment without METRICS_TOKEN cannot poll and records that."""
        _ = self.stage()
        environment = (self.config / "app.env").read_text()
        _ = (self.config / "app.env").write_text(
            "\n".join(
                line for line in environment.splitlines() if not line.startswith("METRICS_TOKEN=")
            )
            + "\n"
        )
        self.runner.metrics_rooms = [5]
        self.execute_quiet(30)
        report = self.report()
        self.assertTrue(report["passed"])
        self.assertIsNone(report["roomsActiveAtReplacement"])
        self.assertEqual(report["quietWaitSeconds"], 0)
        polls = [
            call
            for call in self.runner.calls
            if call[0] == "run" and call[1][-1].endswith("/metrics")
        ]
        self.assertEqual(polls, [])

    def execute_quiet(self, seconds: int) -> None:
        """Run a deployment with a bounded wait for empty rooms."""
        with (
            patch.object(
                sys,
                "argv",
                ["release-public.py", "deploy", REVISION, "--quiet-seconds", str(seconds)],
            ),
            redirect_stdout(io.StringIO()),
        ):
            public.main()

    def assert_failed_rollback(self) -> None:
        """Require successful recovery to retain the original failed release outcome."""
        report = self.report()
        self.assertFalse(report["passed"])
        self.assertTrue(report["rollbackAttempted"])
        self.assertTrue(report["rollbackPassed"])
        self.assertEqual(len(self.app_mutations()), 4)
        self.assertEqual(self.runner.ups, 2)
        self.assertEqual(self.runner.app_image, OLD_IMAGE)
        captures = [
            index
            for index, (kind, args, _) in enumerate(self.runner.calls)
            if kind == "docker" and args[0] == "inspect" and args[-1] == "1" * 64
        ]
        stops = [
            index
            for index, (kind, args, _) in enumerate(self.runner.calls)
            if kind == "compose" and args[0] == "stop"
        ]
        self.assertEqual(len(captures), 1, "Retain the exact failed app state once before rollback")
        self.assertLess(stops[0], captures[0])
        self.assertLess(captures[0], stops[1])
        self.assertTrue(json_object((self.root / "release-state.json").read_text())["finalized"])
        self.assert_config_unchanged()

    def test_failed_rollback_retains_unfinished_journal_and_failure(self) -> None:
        """Failed rollback retains unfinished journal and failure."""
        _ = self.stage()
        self.runner.candidate_unready = True
        self.runner.rollback_unready = True
        with self.assertRaises(public.ReleaseError):
            self.execute()
        report = self.report()
        self.assertFalse(report["passed"])
        self.assertTrue(report["rollbackAttempted"])
        self.assertFalse(report["rollbackPassed"])
        self.assertFalse(json_object((self.root / "release-state.json").read_text())["finalized"])
        self.assertEqual(self.runner.ups, 2)

    def test_rendered_nonimage_change_is_rejected_before_stop_or_backup(self) -> None:
        """Rendered nonimage change is rejected before stop or backup."""
        _ = self.stage()
        self.runner.preview_change = True
        with self.assertRaisesRegex(public.ReleaseError, "beyond the reviewed selection"):
            self.execute()
        self.assertFalse(self.report()["passed"])
        self.assertEqual(self.app_mutations(), [])
        self.assertFalse(any("pg_dump" in args for _, args, _ in self.runner.calls))
        self.assertNotIn("rollbackAttempted", self.report())
        self.assert_config_unchanged()

    def test_unhealthy_database_preflight_leaves_selection_and_app_untouched(self) -> None:
        """Unhealthy database preflight leaves selection and app untouched."""
        _ = self.stage()
        self.runner.database_healthy = False
        with self.assertRaisesRegex(public.ReleaseError, "Database must be healthy"):
            self.execute()
        self.assertEqual(self.app_mutations(), [])
        self.assertNotIn("rollbackAttempted", self.report())
        self.assert_config_unchanged()

    def test_existing_container_config_drift_is_rejected_before_stop(self) -> None:
        """Existing container config drift is rejected before stop."""
        _ = self.stage()
        self.runner.config_hash_mismatch = "postgres"
        with self.assertRaises(public.ReleaseError):
            self.execute()
        self.assertFalse(self.report()["passed"])
        self.assertEqual(self.app_mutations(), [])
        self.assertNotIn("rollbackAttempted", self.report())
        self.assert_config_unchanged()

    def test_environment_without_trailing_newline_updates_exact_selection(self) -> None:
        """Environment without trailing newline updates exact selection."""
        _ = (self.config / "app.env").write_text(
            f"RUN_MIGRATIONS=false\nSIMPLESTCHAT_IMAGE={OLD_IMAGE}"
        )
        _, candidate = public.candidate_selection(
            self.runner, NEW_IMAGE, {"serverImage": OLD_IMAGE}
        )
        self.assertEqual(
            candidate.decode(), f"RUN_MIGRATIONS=false\nSIMPLESTCHAT_IMAGE={NEW_IMAGE}\n"
        )
        self.assertNotIn(
            NEW_IMAGE,
            (self.config / "app.env").read_text(),
            "Preview cannot alter installed selection",
        )

    def test_failed_validation_container_retains_journal_and_blocks_another_attempt(self) -> None:
        """Failed validation container retains journal and blocks another attempt."""
        self.runner.validation_exit = b"1"
        with self.assertRaisesRegex(public.ReleaseError, "Packaged migration validation failed"):
            self.execute("stage")
        self.assertFalse(self.report()["passed"])
        state = json_object((self.root / "release-state.json").read_text())
        self.assertFalse(state["finalized"])
        self.assertEqual(state["phase"], "validate_image")
        self.assertTrue((self.runner.attempt / "validation-name.txt").is_file())
        calls_before = list(self.runner.calls)
        with self.assertRaisesRegex(public.ReleaseError, "unfinished release"):
            self.execute("stage")
        self.assertEqual(self.runner.calls, calls_before)
        self.assertEqual(self.app_mutations(), [])
        self.assert_config_unchanged()

    def reboot_state(
        self, *, phase: str = "await_reboot", boot: str = "11111111-1111-1111-1111-111111111111"
    ) -> None:
        """Install a prepared reboot journal to exercise shared operation ownership."""
        _ = (self.root / "release-state.json").write_text(
            json.dumps(
                {
                    "schemaVersion": 1,
                    "finalized": False,
                    "phase": phase,
                    "bootId": boot,
                }
            )
        )

    def test_reboot_recovery_cannot_bypass_an_unfinished_benchmark(self) -> None:
        """Reboot recovery cannot bypass an unfinished benchmark."""
        self.reboot_state()
        _ = (self.work / "current.json").write_text('{"schemaVersion":1,"finalized":false}')
        for options in ({"after_reboot": True}, {"cancel_reboot": True}):
            with self.subTest(options=options), patch.object(public, "boot_id") as boot:
                with (
                    self.assertRaisesRegex(public.ReleaseError, "unfinished benchmark"),
                    public.workload_lock(**options),
                ):
                    self.fail("A reboot recovery action cannot bypass benchmark cleanup")
                boot.assert_not_called()
        self.assertEqual(self.runner.calls, [])

    def test_ordinary_release_cannot_bypass_a_prepared_reboot(self) -> None:
        """Ordinary release cannot bypass a prepared reboot."""
        self.reboot_state()
        with (
            self.assertRaisesRegex(public.ReleaseError, "unfinished release"),
            public.workload_lock(),
        ):
            self.fail("Only explicit recovery may enter a pending reboot journal")
        self.assertEqual(self.runner.calls, [])

    def test_after_reboot_requires_a_prepared_journal_and_changed_boot_identity(self) -> None:
        """After reboot requires a prepared journal and changed boot identity."""
        before = "11111111-1111-1111-1111-111111111111"
        after = "22222222-2222-2222-2222-222222222222"
        self.reboot_state(boot=before)
        with (
            patch.object(public, "boot_id", return_value=after),
            public.workload_lock(after_reboot=True),
        ):
            pass
        for phase, boot, current in (
            ("await_reboot", before, before),
            ("replace_application", before, after),
            ("await_reboot", "invalid-boot", after),
        ):
            self.reboot_state(phase=phase, boot=boot)
            with (
                self.subTest(phase=phase, boot=boot, current=current),
                patch.object(public, "boot_id", return_value=current),
                self.assertRaisesRegex(public.ReleaseError, "unfinished release"),
                public.workload_lock(after_reboot=True),
            ):
                self.fail("Unexpected release journal cannot use the reboot exception")

    def test_cancel_reboot_requires_the_same_boot_and_exclusive_recovery_mode(self) -> None:
        """Cancel reboot requires the same boot and exclusive recovery mode."""
        before = "11111111-1111-1111-1111-111111111111"
        self.reboot_state(boot=before)
        with (
            patch.object(public, "boot_id", return_value=before),
            public.workload_lock(cancel_reboot=True),
        ):
            pass
        with (
            patch.object(public, "boot_id", return_value="22222222-2222-2222-2222-222222222222"),
            self.assertRaisesRegex(public.ReleaseError, "unfinished release"),
            public.workload_lock(cancel_reboot=True),
        ):
            self.fail("Cancellation cannot cross a reboot")
        with (
            self.assertRaisesRegex(public.ReleaseError, "Choose one"),
            public.workload_lock(after_reboot=True, cancel_reboot=True),
        ):
            self.fail("Conflicting recovery modes must be rejected")


@dataclass(slots=True, kw_only=True)
class ChildProcess:
    """Model one owned subprocess without creating an operating-system process."""

    pid: int = 12345
    returncode: int | None = 0
    timed_out: bool = False
    signals: list[int] = field(default_factory=list)
    waits: list[float] = field(default_factory=list)

    def communicate(self, _input: bytes | None = None, *, timeout: float) -> tuple[None, None]:
        """Model a completed command or the caller's configured timeout."""
        if self.timed_out:
            command = "fixture"
            raise subprocess.TimeoutExpired(command, timeout)
        return None, None

    def poll(self) -> int | None:
        """Return whether the owned child still requires cleanup."""
        return self.returncode

    def send_signal(self, signum: int) -> None:
        """Record direct-child cleanup without touching a real PID."""
        self.signals.append(signum)

    def wait(self, *, timeout: float) -> int:
        """Record bounded exit observation after the cleanup signal."""
        self.waits.append(timeout)
        self.returncode = 0
        return 0


@dataclass(slots=True)
class ProcessCapture:
    """Retain exactly the stream handles and environment passed to a fake child."""

    stdin: IO[bytes] | None = None
    stdout: IO[bytes] | None = None
    environment: dict[str, str] = field(default_factory=dict)
    start_new_session: bool = False


@final
class PublicRunnerTests(unittest.TestCase):
    """Exercise private command I/O and cleanup with an in-memory child model."""

    def __init__(self, methodName: str = "runTest") -> None:  # noqa: N803 - unittest's public constructor keyword.
        """Keep collection free of resource allocation and external commands."""
        super().__init__(methodName)
        self.temporary: tempfile.TemporaryDirectory[str] | None = None
        self.directory = Path()
        self.runner = public.Runner(self.directory)

    @override
    def setUp(self) -> None:
        """Create isolated fixtures and register reversible test substitutions."""
        self.temporary = tempfile.TemporaryDirectory(prefix="simplestchat-release-runner-test.")
        self.directory = Path(self.temporary.name)
        self.runner = public.Runner(self.directory)

    @override
    def tearDown(self) -> None:
        """Restore process state and remove only owned temporary fixtures."""
        if self.temporary is not None:
            self.temporary.cleanup()

    def test_backup_streams_through_files_without_loading_command_output(self) -> None:
        """Backup streams through files without loading command output."""
        source = self.directory / "input.dump"
        target = self.directory / "output.dump"
        _ = source.write_bytes(b"fixture database archive")
        child = ChildProcess()
        captured = ProcessCapture()

        def spawn(
            _args: Sequence[str],
            *,
            stdin: IO[bytes] | int,
            stdout: IO[bytes],
            stderr: IO[bytes],
            env: dict[str, str],
            start_new_session: bool,
        ) -> ChildProcess:
            assert not isinstance(stdin, int)
            assert not stderr.closed
            captured.stdin, captured.stdout = stdin, stdout
            captured.environment = env
            captured.start_new_session = start_new_session
            _ = stdout.write(stdin.read())
            return child

        with patch.object(subprocess, "Popen", side_effect=spawn):
            result = self.runner.run(
                ["fixture-no-execution"], input_path=source, output_path=target
            )
        self.assertEqual(result, b"")
        self.assertEqual(target.read_bytes(), source.read_bytes())
        assert captured.stdin is not None
        assert captured.stdout is not None
        self.assertTrue(captured.stdin.closed)
        self.assertTrue(captured.stdout.closed)
        self.assertEqual(captured.environment, public.ENV)
        self.assertTrue(captured.start_new_session)

    def test_oversized_inspection_is_retained_but_not_returned(self) -> None:
        """Oversized inspection is retained but not returned."""
        child = ChildProcess()

        def spawn(
            _args: Sequence[str],
            *,
            stdin: IO[bytes] | int,
            stdout: IO[bytes],
            stderr: IO[bytes],
            env: dict[str, str],
            start_new_session: bool,
        ) -> ChildProcess:
            assert stdin == subprocess.DEVNULL
            assert not stderr.closed
            assert env == public.ENV
            assert start_new_session
            _ = stdout.write(b"x" * (2 * 1024 * 1024 + 1))
            return child

        with (
            patch.object(subprocess, "Popen", side_effect=spawn),
            self.assertRaisesRegex(public.ReleaseError, "exceeded its bound"),
        ):
            _ = self.runner.run(["fixture-no-execution"])
        self.assertEqual((self.directory / "001.stdout").stat().st_size, 2 * 1024 * 1024 + 1)

    def test_timeout_cleanup_falls_back_to_owned_child_without_group_probes(self) -> None:
        """Timeout cleanup falls back to owned child without group probes."""
        child = ChildProcess(returncode=None, timed_out=True)
        with (
            patch.object(subprocess, "Popen", return_value=child),
            patch.object(os, "killpg", side_effect=PermissionError("fixture")) as group,
            self.assertRaises(subprocess.TimeoutExpired),
        ):
            _ = self.runner.run(["fixture-no-execution"], timeout=1)
        group.assert_called_once_with(12345, signal.SIGTERM)
        self.assertEqual(child.signals, [signal.SIGTERM])
        self.assertEqual(child.waits, [10])


if __name__ == "__main__":
    _ = unittest.main()
