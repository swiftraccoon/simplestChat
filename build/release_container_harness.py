"""Exercise the unchanged release CLI on a fresh disposable Linux Docker host.

Requires root and explicit --disposable-host. Never use this on the public VPS.
The fixture occupies the production paths only after proving they do not exist,
and refuses any existing container. Services publish only to loopback. Private
fixture files and images remain for inspection; only verified owned containers,
their network and the fixture CA trust entry are removed. report.json is the
only artifact suitable for CI upload. No reboot or physical media is exercised.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Protocol, TypedDict, Unpack

import release_public as release
from release_artifact import sha256_file, validate_manifest, verify_archive
from release_container_fixture import LABEL, FixtureIdentity, render_fixture
from release_json import (
    JsonObject,
    JsonValue,
    boolean_value,
    decode_json,
    integer_value,
    object_value,
    string_value,
)

if TYPE_CHECKING:
    from collections.abc import Mapping, Sequence
    from types import FrameType

PROJECT = Path(__file__).resolve().parents[1]
FILES = PROJECT / "ops/ansible/files"

CONFIG, ROOT, WORK = release.CONFIG, release.ROOT, release.WORK
FIXED_PATHS = (CONFIG, ROOT, WORK, Path("/srv/simplestchat-bench"))
POSTGRES = (
    "docker.io/library/postgres:18.6-bookworm@sha256:"
    + "1c59e2c3c818eaa0f0628f695b36e7c9e362d6b219b36a54a32df645cbd7e1af"
)
CADDY = (
    "docker.io/library/caddy:2.11.4-alpine@sha256:"
    + "5f5c8640aae01df9654968d946d8f1a56c497f1dd5c5cda4cf95ab7c14d58648"
)
FAILURE_MARKER = "simplestchat-disposable-release-startup-failure"
ID = re.compile(r"[a-f0-9]{64}")
MIN_PRINTABLE = 32
DELETE_CHARACTER = 127
MAX_VALIDATOR_NAME = 128
MAX_EXIT_STATUS = 255
MAX_VALIDATOR_ERROR = 2048
MAX_VALIDATOR_STDERR = 4096
type Snapshot = dict[str, tuple[JsonValue, JsonValue, JsonValue, JsonValue]]


class CommandOptions(TypedDict, total=False):
    """The complete bounded command invocation surface used by real and fake runners."""

    timeout: int
    success: bool
    input_path: Path | None
    pass_fds: tuple[int, ...]


class CommandResult(Protocol):
    """A command's exit code and retained private output locations."""

    @property
    def code(self) -> int:
        """Return the original child exit status."""
        ...

    @property
    def error(self) -> Path:
        """Locate the retained stderr without exposing it to public reports."""
        ...

    def text(self) -> str:
        """Read only bounded private inspection output."""
        ...


class CommandRunner(Protocol):
    """Only explicit local commands and their settlement evidence may drive a fixture."""

    settlement_unconfirmed: bool
    last_failure: JsonObject | None

    def run(self, args: list[str], **options: Unpack[CommandOptions]) -> CommandResult:
        """Execute one explicit host command with bounded cleanup."""
        ...

    def docker(self, *args: str, **options: Unpack[CommandOptions]) -> CommandResult:
        """Execute one explicit Docker command on the selected local engine."""
        ...

    def compose(self, *args: str, **options: Unpack[CommandOptions]) -> CommandResult:
        """Execute one fixed-project Compose command."""
        ...


class CheckError(RuntimeError):
    """A fixture assertion failed; retain the original evidence."""


def require(condition: object, message: str) -> None:
    """Refuse an unsafe or unverified fixture transition."""
    if not condition:
        raise CheckError(message)


def write_new(path: Path, value: JsonValue, mode: int = 0o600) -> None:
    """Publish new evidence exclusively without replacing retained files."""
    data = json.dumps(value, indent=2) + "\n" if isinstance(value, (dict, list)) else value
    if not isinstance(data, str):
        message = "Fixture evidence must be text or a JSON object/array"
        raise TypeError(message)
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
    with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
        _ = stream.write(data)


def create_data_directory(path: Path, uid: int, mode: int) -> None:
    """Create an owned data directory with explicit permissions independent of umask."""
    path.mkdir(mode=mode)
    os.chown(path, uid, uid)
    # mkdir's mode is filtered by the private process umask. The
    # application must still traverse the UID-999 socket directory.
    path.chmod(mode)


def fresh_output(value: str) -> Path:
    """Reserve private fresh evidence outside every fixture-owned service path."""
    path = Path(value)
    require(
        path.is_absolute() and path.parent.is_dir() and path.parent.resolve() == path.parent,
        "Output must have an existing absolute, non-symlink parent",
    )
    require(
        not any(ord(char) < MIN_PRINTABLE or ord(char) == DELETE_CHARACTER for char in str(path)),
        "Invalid output path",
    )
    require(not path.exists() and not path.is_symlink(), "Output must be a fresh directory")
    require(
        not any(path == fixed or path.is_relative_to(fixed) for fixed in FIXED_PATHS),
        "Output must be separate from fixture service paths",
    )
    path.mkdir(mode=0o700)
    (path / "private").mkdir(mode=0o700)
    return path


def host_preflight() -> None:
    """Refuse non-disposable hosts before starting any child process or contacting Docker."""
    require(
        sys.platform == "linux" and platform.machine() in ("x86_64", "amd64") and os.geteuid() == 0,
        "Use a fresh disposable Linux/amd64 host as root; never the public VPS",
    )
    for path in FIXED_PATHS:
        require(
            not path.exists() and not path.is_symlink(),
            "Existing service or benchmark path; refusing fixture setup",
        )
    for command in (
        "/usr/bin/docker",
        "/usr/bin/curl",
        "/usr/bin/openssl",
        "/usr/bin/nsenter",
        "/usr/sbin/update-ca-certificates",
    ):
        require(os.access(command, os.X_OK), "Required Docker, TLS or namespace tooling is missing")
    # No probing an existing service: bind checks only, before creating fixtures.
    for port, kind in (
        (3000, socket.SOCK_STREAM),
        (443, socket.SOCK_STREAM),
        (40000, socket.SOCK_DGRAM),
    ):
        with socket.socket(socket.AF_INET, kind) as probe:
            probe.bind(("127.0.0.1", port))
    require(
        shutil.disk_usage("/srv").free >= 4 * 1024**3,
        "At least 4 GiB of free fixture space is required",
    )


@dataclass(frozen=True)
class Command:
    """The original exit code and immutable private output files for one child."""

    code: int
    output: Path
    error: Path

    def text(self) -> str:
        """Read one bounded textual inspection result, never arbitrary growing logs."""
        require(self.output.stat().st_size <= 2 * 1024**2, "Inspection output exceeds its bound")
        return self.output.read_text(encoding="utf-8").strip()


class Commands:
    """Each command has bounded owned-process cleanup and private immutable logs."""

    def __init__(self, directory: Path) -> None:
        """Keep original output and cleanup settlement beneath the owned evidence directory."""
        self.directory: Path = directory
        self.sequence: int = 0
        self.settlement_unconfirmed: bool = False
        self.last_failure: JsonObject | None = None

    def run(self, args: list[str], **options: Unpack[CommandOptions]) -> Command:
        """Run explicit argv and retain uncertainty before stopping an interrupted child."""
        timeout = options.get("timeout", 30)
        success = options.get("success", True)
        input_path = options.get("input_path")
        pass_fds = options.get("pass_fds", ())
        self.sequence += 1
        prefix = self.directory / f"command-{self.sequence:03d}"
        output, error = prefix.with_suffix(".stdout"), prefix.with_suffix(".stderr")
        process: subprocess.Popen[bytes] | None = None
        started = time.monotonic()
        source = input_path.open("rb") if input_path else None
        try:
            with output.open("xb") as stdout, error.open("xb") as stderr:
                # Explicit argv targets the disposable host; no shell is involved.
                process = subprocess.Popen(  # noqa: S603
                    args,
                    stdin=source or subprocess.DEVNULL,
                    stdout=stdout,
                    stderr=stderr,
                    env=release.ENV,
                    start_new_session=True,
                    pass_fds=pass_fds,
                )
                _ = process.wait(timeout=timeout)
        finally:
            if source:
                source.close()
            if process is not None and process.poll() is None:
                # Killing a Docker client or release supervisor cannot prove
                # daemon-side work (or a separately sessioned child) has ended.
                # No fixture cleanup may race that uncertain operation.
                self.settlement_unconfirmed = True
                release.Runner.stop(process)
            record: JsonObject = {
                "command": list(args),
                "timeoutSeconds": timeout,
                "exitStatus": process.returncode if process else None,
                "seconds": round(time.monotonic() - started, 3),
            }
            write_new(prefix.with_suffix(".json"), record)
        if process is None or process.returncode is None:
            message = "Owned command exited without an original status"
            raise CheckError(message)
        result = Command(process.returncode, output, error)
        if result.code != 0:
            # Fixed operation names only: never copy resolved config, command
            # arguments, environment values or database content into CI output.
            operation = Path(args[0]).name
            if args[: len(release.DOCKER)] == release.DOCKER:
                verb = args[len(release.DOCKER)]
                operation = "docker " + verb
                if verb in ("image", "network", "buildx"):
                    operation += " " + args[len(release.DOCKER) + 1]
            elif str(FILES / "release-public.py") in args:
                operation = "release-public.py " + args[-2]
            self.last_failure = {
                "number": self.sequence,
                "operation": operation,
                "exitStatus": result.code,
            }
        require(
            not success or result.code == 0,
            f"Command {self.sequence:03d} failed; inspect private logs",
        )
        return result

    def docker(self, *args: str, **kwargs: Unpack[CommandOptions]) -> Command:
        """Route one explicit command to the fixed local Docker endpoint."""
        return self.run([*release.DOCKER, *args], **kwargs)

    def compose(self, *args: str, **kwargs: Unpack[CommandOptions]) -> Command:
        """Scope Compose commands to the disposable fixture's fixed project and files."""
        return self.docker(
            "compose",
            "--project-name",
            "simplestchat-public",
            "--project-directory",
            str(CONFIG),
            "--env-file",
            str(CONFIG / "app.env"),
            "-f",
            str(CONFIG / "compose.public.yml"),
            *args,
            **kwargs,
        )


def inspect_owned(commands: CommandRunner, identity: str, token: str) -> JsonObject:
    """Verify immutable container identity and every required ownership label."""
    require(ID.fullmatch(identity), "Container identity must be exact")
    value = object_value(
        decode_json(
            commands.docker(
                "inspect",
                "--format",
                '{"id":{{json .Id}},"image":{{json .Image}},"state":{{json .State}},'
                + '"restarts":{{.RestartCount}},"labels":{{json .Config.Labels}}}',
                identity,
            ).text()
        )
    )
    labels = object_value(value["labels"])
    require(
        value["id"] == identity
        and labels.get(LABEL) == token
        and labels.get("com.docker.compose.project") == "simplestchat-public"
        and labels.get("com.docker.compose.service")
        in ("simplestchat", "postgres", "caddy", "migrate"),
        "Container ownership does not match this fixture",
    )
    return value


def assert_healthy_exit(value: JsonObject, *, deliberate_failure: bool = False) -> None:
    """Permit only a clean shutdown or the one deliberately selected fixture failure."""
    state = object_value(value["state"])
    require(
        not state["Running"]
        and not state["OOMKilled"]
        and not state["Error"]
        and state["ExitCode"] == (42 if deliberate_failure else 0),
        "Owned container did not exit as expected",
    )


class Harness:
    """Own the complete disposable-host fixture lifecycle and its original outcome."""

    def __init__(self, output: Path, image: str) -> None:
        """Initialize evidence without touching Docker, services, or fixture paths."""
        self.output: Path = output
        self.image: str = image
        self.private: Path = output / "private"
        self.commands: CommandRunner = Commands(self.private)
        self.token: str = uuid.uuid4().hex
        self.created_paths: bool = False
        self.ca_path: Path | None = None
        self.ca_digest: str | None = None
        self.base_image: str = ""
        self.base_tag: str = ""
        self.fixture_images: dict[str, dict[str, str]] = {}
        self.report: JsonObject = {
            "schemaVersion": 1,
            "passed": False,
            "phase": "preflight",
            "cases": {},
            "cleanupPassed": False,
            "execution": "real-containers",
            "rebootTested": False,
            "derivedImagesAreTestFixtures": True,
        }

    def service(self, name: str) -> JsonObject:
        """Inspect one running service only after verifying the fixture ownership labels."""
        identity = self.commands.compose("ps", "--status", "running", "--quiet", name).text()
        return inspect_owned(self.commands, identity, self.token)

    def selection(self) -> dict[str, bytes]:
        """Snapshot exact configuration bytes without exposing their contents."""
        return {name: (CONFIG / name).read_bytes() for name in release.SELECTION}

    def snapshot(self) -> Snapshot:
        """Capture immutable service identities for interruption and rollback comparisons."""
        return {
            name: release.stable_container(self.service(name))
            for name in ("simplestchat", "postgres", "caddy")
        }

    def sql(
        self, text: str | None = None, *, input_path: Path | None = None, success: bool = True
    ) -> CommandResult:
        """Execute bounded fixture SQL through the verified PostgreSQL container."""
        identity = string_value(self.service("postgres")["id"])
        args = [
            "exec",
            "--interactive",
            "--user",
            "999:999",
            identity,
            "timeout",
            "--kill-after=2s",
            "15s",
            "psql",
            "--no-psqlrc",
            "--set",
            "ON_ERROR_STOP=on",
            "--host",
            "/run/simplestchat-postgres",
            "--username",
            "postgres",
            "--dbname",
            "simplestchat",
            "--tuples-only",
            "--no-align",
        ]
        if text is not None:
            args.extend(["--command", text])
        return self.commands.docker(*args, input_path=input_path, success=success)

    def ready(self, *, migration: str | None = None, seconds: int = 45) -> None:
        """Bound readiness checks and pin an isolated migration network namespace."""
        deadline = time.monotonic() + seconds
        while True:
            descriptor = None
            try:
                args = [
                    "/usr/bin/curl",
                    "--disable",
                    "--noproxy",
                    "*",
                    "--proto",
                    "=https" if not migration else "=http",
                    "--fail",
                    "--silent",
                    "--show-error",
                    "--max-time",
                    "2",
                    "http://127.0.0.1:3000/ready" if migration else "https://localhost/ready",
                ]
                if migration:
                    state = object_value(
                        inspect_owned(self.commands, migration, self.token)["state"]
                    )
                    require(
                        state["Running"] and integer_value(state["Pid"]) > 1,
                        "Migration container stopped before readiness",
                    )
                    descriptor = os.open(f"/proc/{state['Pid']}/ns/net", os.O_RDONLY)
                    require(
                        object_value(inspect_owned(self.commands, migration, self.token)["state"])[
                            "Pid"
                        ]
                        == state["Pid"],
                        "Migration namespace changed",
                    )
                    args = ["/usr/bin/nsenter", f"--net=/proc/self/fd/{descriptor}", "--", *args]
                result = self.commands.run(
                    args,
                    timeout=5,
                    success=False,
                    pass_fds=() if descriptor is None else (descriptor,),
                )
                if (
                    result.code == 0
                    and object_value(decode_json(result.text())).get("status") == "ready"
                ):
                    return
            finally:
                if descriptor is not None:
                    os.close(descriptor)
            require(time.monotonic() < deadline, "Fixture readiness deadline exceeded")
            time.sleep(0.5)

    def derivative(self, kind: str) -> dict[str, str]:
        """Derive an isolated fixture image from the same checked local production image."""
        revision = hashlib.sha256(f"disposable-release:{self.token}:{kind}".encode()).hexdigest()[
            :40
        ]
        tag = f"simplestchat-release/production:{revision}"
        directory = self.private / kind
        directory.mkdir(mode=0o700)
        require(
            self.commands.docker("image", "inspect", "--format", "{{.Id}}", self.base_tag).text()
            == self.base_image,
            "Fixture base tag changed before build",
        )
        dockerfile = (
            f"FROM {self.base_tag}\nLABEL org.opencontainers.image.revision={revision}\n"
            + f"LABEL {LABEL}={self.token}\n"
        )
        if kind == "failed":
            write_new(
                directory / "failed-server", f"#!/bin/sh\necho '{FAILURE_MARKER}' >&2\nexit 42\n"
            )
            dockerfile += "COPY --chown=10001:10001 --chmod=0755 failed-server /app/simplestChat\n"
        write_new(directory / "Dockerfile", dockerfile)
        _ = self.commands.docker(
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
            tag,
            str(directory),
            timeout=90,
        )
        require(
            self.commands.docker("image", "inspect", "--format", "{{.Id}}", self.base_tag).text()
            == self.base_image,
            "Fixture base tag changed during build",
        )
        identity = self.commands.docker("image", "inspect", "--format", "{{.Id}}", tag).text()
        require(release.ID.fullmatch(identity), "Fixture image has no immutable identity")
        self.fixture_images[kind] = {"revision": revision, "tag": tag, "image": identity}
        return self.fixture_images[kind]

    def artifact(self, candidate: Mapping[str, str]) -> None:
        """Export and validate a fixture-only archive without rebuilding production code."""
        directory = ROOT / "releases" / candidate["revision"]
        directory.mkdir(mode=0o700)
        _ = self.commands.docker(
            "image",
            "save",
            "--platform",
            "linux/amd64",
            "--output",
            str(directory / "image.tar"),
            candidate["tag"],
            timeout=90,
        )
        (directory / "image.tar").chmod(0o600)
        migrations: JsonObject = {
            str(int(path.name.split("_")[0])): hashlib.sha384(path.read_bytes()).hexdigest()
            for path in sorted((PROJECT / "migrations").glob("*.sql"))
        }
        manifest: JsonObject = {
            "schemaVersion": 1,
            "revision": candidate["revision"],
            "platform": "linux/amd64",
            "archiveSha256": sha256_file(directory / "image.tar"),
            "imageTag": candidate["tag"],
            "migrations": migrations,
            "createdAt": release.timestamp(),
        }
        write_new(directory / "release.json", manifest)
        _ = verify_archive(directory / "image.tar", validate_manifest(directory / "release.json"))

    def certificates(self) -> None:
        """Install one verified fixture CA and private localhost certificate."""
        ca = self.private / "ca.crt"
        key = self.private / "ca.key"
        _ = self.commands.run(
            [
                "/usr/bin/openssl",
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-days",
                "1",
                "-subj",
                f"/CN=simplestchat-disposable-{self.token}",
                "-addext",
                "basicConstraints=critical,CA:TRUE",
                "-keyout",
                str(key),
                "-out",
                str(ca),
            ]
        )
        _ = self.commands.run(
            [
                "/usr/bin/openssl",
                "req",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-subj",
                "/CN=localhost",
                "-keyout",
                str(CONFIG / "fixture.key"),
                "-out",
                str(self.private / "server.csr"),
            ]
        )
        write_new(
            self.private / "server.ext",
            "subjectAltName=DNS:localhost\nbasicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth\n",
        )
        _ = self.commands.run(
            [
                "/usr/bin/openssl",
                "x509",
                "-req",
                "-in",
                str(self.private / "server.csr"),
                "-CA",
                str(ca),
                "-CAkey",
                str(key),
                "-CAcreateserial",
                "-days",
                "1",
                "-extfile",
                str(self.private / "server.ext"),
                "-out",
                str(CONFIG / "fixture.crt"),
            ]
        )
        for name in ("fixture.key", "fixture.crt"):
            os.chown(CONFIG / name, 10001, 10001)
            (CONFIG / name).chmod(0o600)
        self.ca_path = (
            Path("/usr/local/share/ca-certificates") / f"simplestchat-release-test-{self.token}.crt"
        )
        write_new(self.ca_path, ca.read_text(encoding="utf-8"), mode=0o644)
        self.ca_digest = sha256_file(self.ca_path)
        _ = self.commands.run(["/usr/sbin/update-ca-certificates"], timeout=30)

    def setup(self) -> None:
        """Validate the disposable host before creating isolated images and fixed fixture paths."""
        host_preflight()
        require(
            not self.commands.docker("ps", "--all", "--quiet").text(),
            "Existing containers; use an empty disposable engine",
        )
        require(
            not self.commands.docker(
                "network",
                "ls",
                "--quiet",
                "--filter",
                "label=com.docker.compose.project=simplestchat-public",
            ).text(),
            "Existing public Compose network",
        )
        help_text = self.commands.docker("image", "save", "--help").text()
        require(
            re.search(r"^\s+--platform(?:\s|$)", help_text, re.MULTILINE),
            "Docker image export requires --platform support",
        )
        api = self.commands.docker("version", "--format", "{{.Server.APIVersion}}").text()
        require(
            re.fullmatch(r"[0-9]{1,3}\.[0-9]{1,3}", api)
            and tuple(map(int, api.split("."))) >= (1, 48),
            "Docker server API 1.48 or newer is required",
        )
        builder = self.commands.docker("buildx", "inspect", "default").text()
        require(
            re.findall(r"^Driver:\s+(\S+)\s*$", builder, re.MULTILINE) == ["docker"],
            "Fixture builds require the default local Docker builder",
        )
        metadata = object_value(
            decode_json(
                self.commands.docker(
                    "image",
                    "inspect",
                    "--format",
                    '{"id":{{json .Id}},"os":{{json .Os}},'
                    + '"architecture":{{json .Architecture}},"user":{{json .Config.User}},'
                    + '"cmd":{{json .Config.Cmd}},'
                    + '"entrypoint":{{json (index .Config "Entrypoint")}}}',
                    self.image,
                ).text()
            )
        )
        require(
            metadata["os"] == "linux"
            and metadata["architecture"] == "amd64"
            and metadata["user"] == "10001:10001"
            and metadata["cmd"] == ["/app/simplestChat"]
            and metadata["entrypoint"] in (None, [])
            and release.ID.fullmatch(string_value(metadata["id"])),
            "Select the already-built Linux/amd64 production image",
        )
        self.base_image = string_value(metadata["id"])
        self.report["baseImage"] = self.base_image
        revision = self.commands.run(
            ["/usr/bin/git", "-C", str(PROJECT), "rev-parse", "HEAD"]
        ).text()
        require(
            re.fullmatch(r"[a-f0-9]{40}", revision), "Source revision must be an exact Git commit"
        )
        self.report["sourceRevision"] = revision
        self.base_tag = f"simplestchat-release-test/base:{self.token}"
        require(
            not self.commands.docker(
                "image", "ls", "--quiet", "--filter", f"reference={self.base_tag}"
            ).text(),
            "Fixture base tag already exists",
        )
        _ = self.commands.docker("image", "tag", self.base_image, self.base_tag)
        # Prerequisite pulls happen before any service is started.
        for image in (POSTGRES, CADDY):
            _ = self.commands.docker("pull", image, timeout=90)
        baseline = self.derivative("baseline")
        _ = self.derivative("candidate")
        _ = self.derivative("failed")
        for path in (CONFIG, ROOT, WORK):
            path.mkdir(mode=0o700)
            write_new(path / "release-test-owner.json", {"token": self.token})
        self.created_paths = True
        for name in ("releases", "results"):
            (ROOT / name).mkdir(mode=0o700)
        for name, uid, mode in (
            ("postgres", 999, 0o700),
            ("postgres-socket", 999, 0o755),
            ("caddy-data", 10001, 0o700),
            ("caddy-config", 10001, 0o700),
        ):
            create_data_directory(ROOT / name, uid, mode)
        render_fixture(
            CONFIG,
            ROOT,
            FixtureIdentity(baseline["image"], baseline["revision"], self.token, POSTGRES, CADDY),
        )
        for name in ("postgres-admin-password", "pg_hba.conf", "init-database.sql"):
            os.chown(CONFIG / name, 999, 999)
        os.chown(CONFIG / "Caddyfile", 10001, 10001)
        self.certificates()
        _ = self.commands.compose("config", "--quiet")
        self.report["phase"] = "bootstrap"
        _ = self.commands.compose(
            "up",
            "--detach",
            "--no-build",
            "--pull",
            "never",
            "--wait",
            "--wait-timeout",
            "120",
            "postgres",
            timeout=135,
        )
        _ = self.commands.compose(
            "--profile",
            "maintenance",
            "up",
            "--detach",
            "--no-build",
            "--pull",
            "never",
            "--no-deps",
            "migrate",
        )
        migration = string_value(self.service("migrate")["id"])
        self.ready(migration=migration)
        _ = self.commands.compose(
            "--profile", "maintenance", "stop", "--timeout", "30", "migrate", timeout=45
        )
        assert_healthy_exit(inspect_owned(self.commands, migration, self.token))
        _ = self.commands.docker("logs", "--tail", "100", migration)
        _ = self.commands.docker("rm", migration)
        _ = self.sql(input_path=CONFIG / "runtime-grants.sql")
        # The marker is an internally generated UUID hex value, never supplied SQL.
        marker_sql = (
            "CREATE TABLE public.release_fixture_marker (value text NOT NULL); "  # noqa: S608
            + f"INSERT INTO public.release_fixture_marker VALUES ('{self.token}');"  # noqa: S608
        )
        _ = self.sql(marker_sql)
        _ = self.commands.compose(
            "up", "--detach", "--no-build", "--pull", "never", "simplestchat", "caddy", timeout=60
        )
        self.ready()
        for kind in ("candidate", "failed"):
            self.artifact(self.fixture_images[kind])

    def validator_startup_diagnostic(self, attempt: Path, candidate: Mapping[str, str]) -> None:
        """Expose only the fixed, secret-free validator's verified startup state."""
        original_failure = self.commands.last_failure
        unavailable = "ValidatorNameUnavailable"
        try:
            path = attempt / "validation-name.txt"
            metadata = path.lstat()
            require(
                stat.S_ISREG(metadata.st_mode) and 0 < metadata.st_size <= MAX_VALIDATOR_NAME,
                "Validator name must be a bounded regular file",
            )
            descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
            with os.fdopen(descriptor, "r", encoding="utf-8") as source:
                opened = os.fstat(source.fileno())
                require(
                    stat.S_ISREG(opened.st_mode)
                    and opened.st_size <= MAX_VALIDATOR_NAME
                    and (opened.st_dev, opened.st_ino) == (metadata.st_dev, metadata.st_ino),
                    "Validator name changed during inspection",
                )
                name = source.read(MAX_VALIDATOR_NAME + 1).strip()
            require(
                re.fullmatch(r"scpub-release-validate-[a-f0-9]{32}", name), "Invalid validator name"
            )
            unavailable = "ValidatorInspectionUnavailable"
            result = self.commands.docker(
                "inspect",
                "--format",
                '{"id":{{json .Id}},"name":{{json .Name}},"image":{{json .Image}},'
                + '"user":{{json .Config.User}},"entrypoint":{{json .Config.Entrypoint}},'
                + '"network":{{json .HostConfig.NetworkMode}},'
                + '"readOnly":{{json .HostConfig.ReadonlyRootfs}},"state":{{json .State}}}',
                name,
                timeout=10,
                success=False,
            )
            require(result.code == 0, "Validator inspection failed")
            value = object_value(decode_json(result.text()))
            unavailable = "ValidatorOwnershipMismatch"
            require(
                ID.fullmatch(string_value(value.get("id")))
                and value.get("name") == "/" + name
                and value.get("image") == candidate["image"]
                and value.get("user") == "10001:10001"
                and value.get("entrypoint") == ["/usr/bin/timeout"]
                and value.get("network") == "none"
                and value.get("readOnly") is True,
                "Validator ownership does not match the failed release",
            )
            unavailable = "ValidatorStateUnavailable"
            state = object_value(value["state"])
            exit_code = integer_value(state.get("ExitCode"))
            oom_killed = boolean_value(state.get("OOMKilled"))
            error_text = string_value(state.get("Error"))
            status = string_value(state.get("Status"))
            require(
                0 <= exit_code <= MAX_EXIT_STATUS
                and status
                in ("created", "running", "paused", "restarting", "removing", "exited", "dead"),
                "Validator startup state is malformed",
            )
            diagnostic: JsonObject = {
                "diagnosticUnavailable": False,
                "exitCode": exit_code,
                "status": status,
                "oomKilled": oom_killed,
                "error": error_text[:MAX_VALIDATOR_ERROR],
                "errorTruncated": len(error_text) > MAX_VALIDATOR_ERROR,
            }
            self.report["validatorStartup"] = diagnostic
            try:
                # This exact verified validator runs only the fixed checksum
                # command without environment files or secret mounts. Its
                # stderr is useful when execution fails after OCI startup.
                logs = self.commands.docker(
                    "logs", "--tail", "20", string_value(value["id"]), timeout=10, success=False
                )
                require(logs.code == 0, "Validator log retrieval failed")
                metadata = logs.error.lstat()
                require(stat.S_ISREG(metadata.st_mode), "Validator stderr must be a regular file")
                descriptor = os.open(logs.error, os.O_RDONLY | os.O_NOFOLLOW)
                with os.fdopen(descriptor, "rb") as source:
                    opened = os.fstat(source.fileno())
                    require(
                        stat.S_ISREG(opened.st_mode)
                        and (opened.st_dev, opened.st_ino) == (metadata.st_dev, metadata.st_ino),
                        "Validator stderr changed during inspection",
                    )
                    stderr = source.read(MAX_VALIDATOR_STDERR + 1)
                diagnostic["runtimeLogs"] = {
                    "diagnosticUnavailable": False,
                    "stderr": stderr[:MAX_VALIDATOR_STDERR].decode("utf-8", errors="replace"),
                    "stderrTruncated": len(stderr) > MAX_VALIDATOR_STDERR,
                }
            except Exception:  # noqa: BLE001 - Diagnostics never replace the original failure.
                # Secondary diagnostics must not erase verified startup state
                # or publish a failed Docker command's own raw error output.
                diagnostic["runtimeLogs"] = {"diagnosticUnavailable": True}
        except Exception:  # noqa: BLE001 - Publish only the fixed unavailable reason.
            # Missing or changed resources are not evidence of a successful
            # validator. Never replace the original failed release result.
            self.report["validatorStartup"] = {"diagnosticUnavailable": True, "reason": unavailable}
        finally:
            self.commands.last_failure = original_failure

    def invoke_release(
        self, action: str, candidate: Mapping[str, str], *, failure: bool = False
    ) -> tuple[Path, JsonObject]:
        """Run one real release attempt and preserve its original, identity-bound result."""
        before = set((ROOT / "results").glob("release.*"))
        command = self.commands.run(
            [sys.executable, "-B", str(FILES / "release-public.py"), action, candidate["revision"]],
            timeout=180,
            success=False,
        )
        attempts = set((ROOT / "results").glob("release.*")) - before
        require(len(attempts) == 1, "Release must retain exactly one new attempt")
        attempt = attempts.pop()
        outcome = object_value(decode_json((attempt / "outcome.json").read_bytes()))
        self.report["lastRelease"] = {
            key: outcome[key]
            for key in (
                "action",
                "revision",
                "phase",
                "passed",
                "failure",
                "rollbackAttempted",
                "rollbackPassed",
            )
            if key in outcome
        }
        if command.code != 0:
            self.validator_startup_diagnostic(attempt, candidate)
        require(
            outcome["action"] == action and outcome["revision"] == candidate["revision"],
            "Release evidence identity mismatch",
        )
        require(
            (command.code != 0 if failure else command.code == 0)
            and outcome["passed"] is (not failure),
            "Release exit status and original outcome disagree",
        )
        return attempt, outcome

    def continuity(self, before: Snapshot) -> Snapshot:
        """Prove retained dependencies and durable database content survived the release."""
        after = self.snapshot()
        require(
            all(after[name] == before[name] for name in ("postgres", "caddy")),
            "A retained dependency changed",
        )
        require(
            self.sql("SELECT value FROM public.release_fixture_marker").text() == self.token,
            "Durable database marker changed",
        )
        return after

    def check_backup(self, attempt: Path, outcome: JsonObject) -> None:
        """Verify the original backup digest and expected database contents."""
        backup = attempt / "database-before.dump"
        require(
            backup.stat().st_size > 0 and sha256_file(backup) == outcome["backupSha256"],
            "Backup identity mismatch",
        )
        identity = string_value(self.service("postgres")["id"])
        listing = self.commands.docker(
            "exec",
            "--interactive",
            "--user",
            "999:999",
            identity,
            "timeout",
            "--kill-after=2s",
            "15s",
            "pg_restore",
            "--list",
            input_path=backup,
        ).text()
        require(
            "release_fixture_marker" in listing and "_sqlx_migrations" in listing,
            "Live backup omitted expected tables",
        )

    def exercise(self) -> None:
        """Exercise one successful app-only release and one deliberately failed startup rollback."""
        for kind in ("candidate", "failed"):
            candidate = self.fixture_images[kind]
            original, before = self.selection(), self.snapshot()
            self.report["phase"] = "stage_" + kind
            _ = self.invoke_release("stage", candidate)
            require(
                self.selection() == original and self.snapshot() == before,
                "Staging interrupted or changed the running deployment",
            )
            self.report["phase"] = "deploy_" + kind
            started = time.monotonic()
            attempt, outcome = self.invoke_release("deploy", candidate, failure=kind == "failed")
            after = self.continuity(before)
            self.ready()
            self.check_backup(attempt, outcome)
            journal = object_value(decode_json((ROOT / "release-state.json").read_bytes()))
            require(journal["finalized"] is True, "Release cleanup remained unfinished")
            if kind == "failed":
                require(
                    outcome.get("rollbackAttempted") is True
                    and outcome.get("rollbackPassed") is True
                    and journal["phase"] == "rolled_back",
                    "Failed startup did not complete its single rollback",
                )
                require(
                    self.selection() == original
                    and after["simplestchat"][1] == before["simplestchat"][1],
                    "Rollback did not restore the prior selection and image",
                )
                outputs = [
                    path.read_text(encoding="utf-8")
                    for path in attempt.glob("*.stdout")
                    if path.stat().st_size <= 2 * 1024**2
                ]
                require(
                    any(FAILURE_MARKER in text for text in outputs),
                    "Failed startup log was not retained",
                )
                captured: list[JsonObject] = []
                for text in outputs:
                    try:
                        value = decode_json(text)
                    except ValueError:
                        continue
                    if isinstance(value, dict) and value.get("image") == candidate["image"]:
                        state = value.get("state")
                        if isinstance(state, dict):
                            captured.append(state)
                # With unless-stopped, Docker may already be starting the next
                # failed attempt when inspected. Do not require one snapshot to
                # catch exit 42; retain real state plus the deterministic log.
                require(
                    any(
                        type(state.get("ExitCode")) is int and state.get("OOMKilled") is False
                        for state in captured
                    ),
                    "Failed candidate image/state evidence was not retained",
                )
            else:
                require(
                    after["simplestchat"][1] == candidate["image"]
                    and after["simplestchat"][0] != before["simplestchat"][0]
                    and journal["phase"] == "complete",
                    "Successful upgrade did not replace the application",
                )
            object_value(self.report["cases"])[kind] = {
                "passed": True,
                "releasePassed": outcome["passed"],
                "rollbackPassed": outcome.get("rollbackPassed"),
                "dependenciesUnchanged": True,
                "databaseMarkerPreserved": True,
                "backupVerified": True,
                "stageUninterrupted": True,
                "commandSeconds": round(time.monotonic() - started, 3),
            }
        self.exercise_turn_configuration()

    def exercise_turn_configuration(self) -> None:
        """Verify real Compose env_file preview and same-image replacement, without a relay."""
        self.report["phase"] = "enable_turn_configuration"
        before = self.snapshot()
        selected = object_value(decode_json((CONFIG / "images.json").read_bytes()))
        revision = string_value(selected["revision"])
        manifest = validate_manifest(ROOT / "releases" / revision / "release.json")
        attempt = Path(tempfile.mkdtemp(prefix="turn-config.", dir=ROOT / "results"))
        outcome: JsonObject = {}
        turn = release.TurnConfiguration(domain="relay.example.invalid", secret="a" * 64)
        with release.workload_lock():
            release.deploy(release.Runner(attempt), manifest, selected, outcome, turn=turn)
        after = self.continuity(before)
        self.check_backup(attempt, outcome)
        self.ready()
        require(
            after["simplestchat"][1] == before["simplestchat"][1]
            and after["simplestchat"][0] != before["simplestchat"][0],
            "TURN configuration did not replace only the app using the same image",
        )
        configured = object_value(
            object_value(
                object_value(
                    decode_json(self.commands.compose("config", "--format", "json").text())
                )["services"]
            )["simplestchat"]
        )
        require(
            all(
                object_value(configured["environment"])[key] == value
                for key, value in turn.environment().items()
            ),
            "TURN values did not reach the rendered application configuration",
        )
        object_value(self.report["cases"])["turnConfiguration"] = {
            "passed": True,
            "sameImage": True,
            "dependenciesUnchanged": True,
            "databaseMarkerPreserved": True,
            "backupVerified": True,
            "limitation": "Configuration activation only; no TURN connectivity is tested here.",
        }

    def cleanup(self) -> None:
        """Remove only verified owned fixture resources after command settlement is confirmed."""
        failures: list[JsonValue] = []
        if self.commands.settlement_unconfirmed:
            self.report["cleanupPassed"] = False
            self.report["cleanupFailures"] = ["CommandSettlementUnconfirmed"]
            return
        if self.created_paths:
            try:
                identities = (
                    self.commands.docker(
                        "ps",
                        "--all",
                        "--quiet",
                        "--no-trunc",
                        "--filter",
                        f"label={LABEL}={self.token}",
                    )
                    .text()
                    .splitlines()
                )
                containers = [
                    inspect_owned(self.commands, identity, self.token) for identity in identities
                ]
                order = {"simplestchat": 0, "migrate": 1, "caddy": 2, "postgres": 3}
                for value in sorted(
                    containers,
                    key=lambda item: order[
                        string_value(object_value(item["labels"])["com.docker.compose.service"])
                    ],
                ):
                    identity = string_value(value["id"])
                    _ = self.commands.docker("logs", "--tail", "150", identity)
                    if object_value(value["state"])["Running"]:
                        _ = self.commands.docker("stop", "--time", "30", identity, timeout=45)
                    stopped = inspect_owned(self.commands, identity, self.token)
                    assert_healthy_exit(
                        stopped,
                        deliberate_failure=stopped["image"]
                        == self.fixture_images.get("failed", {}).get("image"),
                    )
                    _ = self.commands.docker("rm", identity)
                networks = (
                    self.commands.docker(
                        "network",
                        "ls",
                        "--quiet",
                        "--no-trunc",
                        "--filter",
                        f"label={LABEL}={self.token}",
                    )
                    .text()
                    .splitlines()
                )
                for identity in networks:
                    require(ID.fullmatch(identity), "Network identity must be exact")
                    value = object_value(
                        decode_json(
                            self.commands.docker(
                                "network", "inspect", "--format", "{{json .}}", identity
                            ).text()
                        )
                    )
                    labels = object_value(value["Labels"])
                    require(
                        value["Id"] == identity
                        and labels.get(LABEL) == self.token
                        and labels.get("com.docker.compose.project") == "simplestchat-public"
                        and not value["Containers"],
                        "Network ownership or cleanup is uncertain",
                    )
                    _ = self.commands.docker("network", "rm", identity)
                # Release validators are normally removed by the real helper. Do
                # not guess ownership or delete an ambiguous daemon-side result.
                require(
                    not self.commands.docker("ps", "--all", "--quiet").text(),
                    "Containers remain after fixture cleanup; inspect private evidence",
                )
            except Exception as error:  # noqa: BLE001 - Retain uncertain resources and fixed type.
                failures.append(type(error).__name__)
        if self.ca_path and not self.ca_digest:
            # A partial exclusive write must never be mistaken for complete
            # cleanup. Retain it for inspection without deleting uncertain data.
            failures.append("FixtureCaPublicationIncomplete")
        if self.ca_path and self.ca_digest:
            try:
                require(
                    stat.S_ISREG(self.ca_path.lstat().st_mode)
                    and sha256_file(self.ca_path) == self.ca_digest,
                    "Fixture CA ownership changed",
                )
                self.ca_path.unlink()
                _ = self.commands.run(["/usr/sbin/update-ca-certificates"], timeout=30)
            except Exception as error:  # noqa: BLE001 - Preserve incomplete trust cleanup evidence.
                failures.append(type(error).__name__)
        self.report["cleanupPassed"] = not failures
        self.report["cleanupFailures"] = failures


@dataclass
class Options(argparse.Namespace):
    """Typed command-line selection without dynamically typed namespace attributes."""

    disposable_host: bool = False
    image: str = ""
    output: str = ""


def options(arguments: Sequence[str] | None) -> Options:
    """Require explicit disposable-host intent and an unambiguous local image selector."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--disposable-host", action="store_true", required=True)
    _ = parser.add_argument("--image", required=True)
    _ = parser.add_argument("--output", required=True)
    result = Options()
    _ = parser.parse_args(arguments, namespace=result)
    if not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9._/:@-]{0,255}", result.image):
        parser.error("--image must be one local image selector")
    return result


def main(arguments: Sequence[str] | None = None) -> int:
    """Run once, retain failure and cleanup uncertainty, and print only sanitized evidence."""
    parsed = options(arguments)
    _ = os.umask(0o077)
    output = fresh_output(parsed.output)
    harness = Harness(output, parsed.image)
    started = time.monotonic()

    def interrupted(_number: int, _frame: FrameType | None) -> None:
        message = "Container release attempt interrupted or exceeded its work deadline"
        raise CheckError(message)

    for number in (signal.SIGTERM, signal.SIGINT, signal.SIGALRM):
        _ = signal.signal(number, interrupted)
    _ = signal.alarm(480)
    try:
        harness.setup()
        harness.exercise()
        harness.report["phase"] = "complete"
        harness.report["passed"] = True
    except Exception as error:  # noqa: BLE001 - CLI boundary publishes sanitized errors only.
        harness.report["failure"] = (
            str(error) if isinstance(error, CheckError) else type(error).__name__
        )
    finally:
        _ = signal.alarm(100)
        try:
            harness.cleanup()
        except Exception as error:  # noqa: BLE001 - Cleanup must not obscure the original outcome.
            harness.report["cleanupPassed"] = False
            harness.report["cleanupFailures"] = [type(error).__name__]
        _ = signal.alarm(0)
        harness.report["passed"] = harness.report["passed"] and harness.report["cleanupPassed"]
        harness.report["seconds"] = round(time.monotonic() - started, 3)
        harness.report["lastNonzeroCommand"] = harness.commands.last_failure
        write_new(output / "report.json", harness.report, mode=0o644)
        (output / "report.json").chmod(0o644)
        output.chmod(0o755)
        print(json.dumps(harness.report))  # noqa: T201 - Public sanitized CI report.
        print(  # noqa: T201 - Explicit CLI cleanup summary without private diagnostics.
            "Private fixture data and images retained; verified owned containers "
            + f"and fixture trust entry cleaned: {harness.report['cleanupPassed']}"
        )
    return 0 if harness.report["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
