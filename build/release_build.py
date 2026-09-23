"""Build or export one production-only Linux release, without a registry push.

Requires an already configured local Docker/Buildx installation. The checked-in
Dockerfile remains authoritative for native provenance and dependency checks.
The output is a fresh, private evidence directory; failures are retained and
never retried automatically. No running application container is changed.
An explicit --image-id exports that exact local image without building or pulling;
the caller is responsible for testing that immutable image before publication.
"""

# Fixed operational failure messages are part of the CLI contract.
# ruff: noqa: EM101, EM102, TRY003
import argparse
import fcntl
import hashlib
import json
import os
import re
import shutil
import signal
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
from collections.abc import Generator, Mapping, Sequence
from contextlib import ExitStack, contextmanager
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path, PurePosixPath
from types import FrameType
from typing import Protocol, override

import release_artifact as ARTIFACT  # noqa: N812 -- Keep established helper aliases.
from release_json import JsonObject, JsonValue, decode_json, object_value, string_value

ROOT = Path(__file__).resolve().parents[1]
BENCHMARK_STATE = Path("/run/simplestchat-bench")
RELEASE_STATE = Path("/srv/simplestchat-public/release-state.json")
FIRST_PRINTABLE = 32
PRIVATE_DIRECTORY_MODE = 0o700
PRIVATE_FILE_MODE = 0o600
MAX_RELEASE_STATE_BYTES = 65536
MAX_BENCHMARK_STATE_BYTES = 16384
MAX_IMAGE_LAYERS = 1000
MAX_MIGRATIONS = 1000
MIN_BUILD_SECONDS = 60
MAX_BUILD_SECONDS = 7200


class BuildError(RuntimeError):
    """A failed release step; the caller must inspect its retained evidence."""


def timestamp() -> str:
    """Return the UTC timestamp used in release evidence."""
    return datetime.now(UTC).strftime("%Y-%m-%dT%H:%M:%SZ")


def write_json(path: Path, value: JsonValue | ARTIFACT.Manifest) -> None:
    """Create a new JSON evidence file without overwriting an existing record."""
    with path.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, indent=2, sort_keys=True)
        _ = stream.write("\n")


class RunnerProtocol(Protocol):
    """Subprocess boundary shared with deterministic offline release fixtures."""

    output: Path | None

    def run(  # noqa: PLR0913 -- Explicit boundary options preserve the subprocess contract.
        self,
        argv: Sequence[str],
        *,
        cwd: Path,
        timeout: float = 30,
        env: Mapping[str, str] | None = None,
        allow_failure: bool = False,
        capture: bool = True,
    ) -> tuple[int, str]:
        """Execute a bounded command and return its status and captured text."""
        ...


class OwnedProcess(Protocol):
    """The child-process operations required for bounded cleanup."""

    pid: int

    def wait(self, timeout: float | None = None) -> int:
        """Wait for this owned child to exit."""
        ...

    def send_signal(self, signal: int, /) -> None:
        """Signal only this owned child."""
        ...


@dataclass
class Runner:
    """Bound child process groups and retain each command's original output."""

    output: Path | None = None
    sequence: int = 0

    def run(  # noqa: C901, PLR0913, PLR0912 -- Keep ordered transaction checks together.
        self,
        argv: Sequence[str],
        *,
        cwd: Path,
        timeout: float = 30,
        env: Mapping[str, str] | None = None,
        allow_failure: bool = False,
        capture: bool = True,
    ) -> tuple[int, str]:
        """Execute the requested command and retain its bounded result."""
        with ExitStack() as resources:
            self.sequence += 1
            name = f"{self.sequence:02d}-{Path(argv[0]).name}"
            log_path = None
            if self.output:
                log_path = self.output / f"{name}.log"
                log = resources.enter_context(log_path.open("xb"))
                write_json(
                    self.output / f"{name}.command.json",
                    {"argv": list(argv), "timeoutSeconds": timeout},
                )
            else:
                log = resources.enter_context(tempfile.TemporaryFile())
            started = time.monotonic()
            process: subprocess.Popen[bytes] | None = None
            timed_out = False
            cleanup = None
            cleanup_error = None
            try:
                with log:
                    process = subprocess.Popen(  # noqa: S603 -- Execute explicit argv without a shell at the process boundary.
                        argv,
                        cwd=cwd,
                        env=env,
                        stdin=subprocess.DEVNULL,
                        stdout=log,
                        stderr=subprocess.STDOUT,
                        start_new_session=True,
                    )
                    try:
                        returncode = process.wait(timeout=timeout)
                    except subprocess.TimeoutExpired:
                        timed_out = True
                        raise BuildError(f"Command timed out: {name}") from None
                    if not capture:
                        data = b""
                    elif log_path is not None:
                        if log_path.stat().st_size > 2 * 1024 * 1024:
                            raise BuildError(f"Command inspection output too large: {name}")
                        data = log_path.read_bytes()
                    else:
                        _ = log.seek(0)
                        data = log.read(2 * 1024 * 1024 + 1)
                    if process.returncode and not allow_failure:
                        raise BuildError(f"Command failed ({process.returncode}): {name}")
                    if len(data) > 2 * 1024 * 1024:
                        raise BuildError(f"Command inspection output too large: {name}")
                    return returncode, data.decode("utf-8", errors="strict").strip()
            finally:
                if process is not None and process.poll() is None:
                    try:
                        cleanup = self.stop_process(process)
                    except (OSError, subprocess.TimeoutExpired) as error:
                        cleanup_error = f"{type(error).__name__}: {error}"
                if self.output:
                    write_json(
                        self.output / f"{name}.outcome.json",
                        {
                            "pid": process.pid if process else None,
                            "exitStatus": process.returncode if process else None,
                            "timedOut": timed_out,
                            "elapsedSeconds": round(time.monotonic() - started, 3),
                            "cleanup": cleanup,
                            "cleanupError": cleanup_error,
                        },
                    )
                if cleanup_error:
                    raise BuildError(
                        f"Owned command cleanup is uncertain: {name}; inspect its outcome"
                    )

    @staticmethod
    def stop_process(process: OwnedProcess) -> JsonObject:
        """Terminate the owned child group and record any cleanup escalation."""
        result: JsonObject = {"groupSignalDenied": False, "forcedKill": False}

        def send(signum: signal.Signals) -> None:
            try:
                os.killpg(process.pid, signum)
            except ProcessLookupError:
                # The group may have exited between poll and signal. Popen
                # owns this exact child and rechecks its status before signaling.
                process.send_signal(signum)
            except PermissionError:
                result["groupSignalDenied"] = True
                process.send_signal(signum)

        # Do not use killpg(pid, 0) as a liveness probe: macOS can deny that
        # probe even when terminating/reaping our direct child is permitted.
        send(signal.SIGTERM)
        try:
            _ = process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            result["forcedKill"] = True
            send(signal.SIGKILL)
            _ = process.wait(timeout=5)
        return result


def clean_revision(runner: RunnerProtocol, root: Path) -> str:
    """Require an unchanged checkout and return its exact full revision."""
    _, revision = runner.run(["git", "rev-parse", "--verify", "HEAD"], cwd=root)
    if not re.fullmatch(r"[a-f0-9]{40}", revision):
        raise BuildError("HEAD is not a full Git commit ID")
    _, dirty = runner.run(["git", "status", "--porcelain=v1", "--untracked-files=all"], cwd=root)
    if dirty:
        raise BuildError("Commit or remove checkout changes before building a release")
    return revision


def validate_output(value: str | Path, root: Path, runner: RunnerProtocol) -> Path:
    """Validate a fresh evidence directory outside tracked checkout content."""
    path = Path(value).expanduser().absolute()
    if any(ord(char) < FIRST_PRINTABLE for char in str(path)) or "," in str(path):
        raise BuildError("Output path cannot contain control characters or commas")
    if path.exists() or path.is_symlink():
        raise BuildError("Output directory must not already exist")
    if not path.parent.is_dir() or path.parent.resolve() != path.parent:
        raise BuildError("Output parent must exist and contain no symlink components")
    path = path.parent.resolve() / path.name
    if path.is_relative_to(root):
        status, _ = runner.run(
            ["git", "check-ignore", "--quiet", "--", str(path)], cwd=root, allow_failure=True
        )
        if status != 0:
            raise BuildError("Output inside the checkout must be Git-ignored")
    return path


@contextmanager
def benchmark_guard(  # noqa: C901 -- Keep ordered transaction checks together.
    state: Path = BENCHMARK_STATE, release: Path = RELEASE_STATE
) -> Generator[None]:
    """Honor the host automation lock without creating host state on a Mac."""
    lock = None
    try:
        if state.exists() or state.is_symlink():
            if (
                state.is_symlink()
                or not state.is_dir()
                or state.stat().st_uid != 0
                or state.stat().st_mode & 0o777 != PRIVATE_DIRECTORY_MODE
            ):
                raise BuildError("Cannot verify private benchmark state ownership")
            path = state / "workload.lock"
            if (
                path.is_symlink()
                or not path.is_file()
                or path.stat().st_uid != 0
                or path.stat().st_mode & 0o777 != PRIVATE_FILE_MODE
            ):
                raise BuildError("Cannot verify benchmark workload lock")
            lock = path.open("r+")
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as error:
                raise BuildError("Another simplestChat build or benchmark is active") from error
            record = state / "current.json"
            if record.exists() or record.is_symlink():
                if (
                    record.is_symlink()
                    or not record.is_file()
                    or record.stat().st_uid != 0
                    or record.stat().st_mode & 0o777 != PRIVATE_FILE_MODE
                    or record.stat().st_size > MAX_RELEASE_STATE_BYTES
                ):
                    raise BuildError("Cannot verify benchmark cleanup record")
                values = decode_json(record.read_text())
                if (
                    not isinstance(values, dict)
                    or type(values.get("schemaVersion")) is not int
                    or values.get("schemaVersion") != 1
                    or values.get("finalized") is not True
                ):
                    raise BuildError("Benchmark cleanup is unfinished")
        if release.exists() or release.is_symlink():
            metadata = release.lstat()
            parent = release.parent.lstat()
            if (
                not stat.S_ISREG(metadata.st_mode)
                or metadata.st_uid != 0
                or stat.S_IMODE(metadata.st_mode) != PRIVATE_FILE_MODE
                or metadata.st_size > MAX_BENCHMARK_STATE_BYTES
                or not stat.S_ISDIR(parent.st_mode)
                or parent.st_uid != 0
                or stat.S_IMODE(parent.st_mode) != PRIVATE_DIRECTORY_MODE
            ):
                raise BuildError("Cannot verify private release state ownership")
            release_record = decode_json(release.read_text())
            if (
                not isinstance(release_record, dict)
                or type(release_record.get("schemaVersion")) is not int
                or release_record.get("schemaVersion") != 1
                or release_record.get("finalized") is not True
            ):
                raise BuildError("Release cleanup is unfinished")
        yield
    finally:
        if lock:
            lock.close()


def docker_preflight(  # noqa: C901 -- Keep ordered transaction checks together.
    runner: RunnerProtocol, root: Path, *, require_builder: bool = True
) -> tuple[list[str], dict[str, str]]:
    """Require the local Docker endpoint and supported production export tools."""
    env = dict(os.environ)
    endpoint = env.get("DOCKER_HOST") if not env.get("DOCKER_CONTEXT") else None
    if not endpoint:
        _, endpoint = runner.run(
            ["docker", "context", "inspect", "--format", "{{.Endpoints.docker.Host}}"], cwd=root
        )
    if not re.fullmatch(r"unix:///[^\x00-\x20]+", endpoint):
        raise BuildError("Release builds require a local Unix-socket Docker endpoint")
    for name in (
        "DOCKER_CONTEXT",
        "DOCKER_HOST",
        "DOCKER_TLS_VERIFY",
        "DOCKER_CERT_PATH",
        "DOCKER_API_VERSION",
        "BUILDX_BUILDER",
        "BUILDKIT_HOST",
    ):
        _ = env.pop(name, None)
    docker = ["docker", "--host", endpoint]
    _, save_help = runner.run([*docker, "image", "save", "--help"], cwd=root, env=env)
    if len(re.findall(r"^[ \t]+--platform(?:[ \t]|$)", save_help, re.MULTILINE)) != 1:
        raise BuildError("Docker CLI must support image save --platform before building a release")
    _, server_api = runner.run(
        [*docker, "version", "--format", "{{.Server.APIVersion}}"], cwd=root, env=env
    )
    version = re.fullmatch(r"(0|[1-9][0-9]{0,2})\.(0|[1-9][0-9]{0,2})", server_api)
    if version is None:
        raise BuildError(
            "Cannot validate the Docker server API version for platform-specific image export"
        )
    if tuple(int(part) for part in version.groups()) < (1, 48):
        raise BuildError("Docker server API 1.48 or newer is required for image save --platform")
    for label in ("com.docker.compose.project=simplestchat-public", "simplestchat.benchmark.run"):
        _, running = runner.run(
            [*docker, "ps", "--quiet", "--filter", f"label={label}"], cwd=root, env=env
        )
        if running:
            raise BuildError(
                "Public chat or a private benchmark is running on this Docker endpoint"
            )
    if require_builder:
        _, builder = runner.run([*docker, "buildx", "inspect", "default"], cwd=root, env=env)
        if re.findall(r"^Driver:\s+(\S+)\s*$", builder, re.MULTILINE) != ["docker"]:
            raise BuildError(
                "The default Buildx builder must use the inspected local Docker driver"
            )
    return docker, env


def select_existing_image(  # noqa: PLR0913, PLR0917 -- Explicit boundary options preserve the subprocess contract.
    runner: RunnerProtocol,
    docker: list[str],
    env: dict[str, str],
    root: Path,
    image_id: str,
    revision: str,
    tag: str,
) -> list[str]:
    """Bind the release tag to an explicitly selected, matching immutable image.

    No image is built, pulled or run. Existing release tags may be reused only
    when they already identify the selected image, never silently replaced.
    """
    _, encoded = runner.run(
        [
            *docker,
            "image",
            "inspect",
            "--format",
            '{"id":{{json .Id}},"os":{{json .Os}},"architecture":{{json .Architecture}},'
            + '"user":{{json .Config.User}},"labels":{{json .Config.Labels}},'
            + '"rootfs":{{json .RootFS}},"cmd":{{json .Config.Cmd}},'
            + '"entrypoint":{{json (index .Config "Entrypoint")}}}',
            image_id,
        ],
        cwd=root,
        env=env,
    )
    value = object_value(decode_json(encoded))
    labels = value.get("labels")
    if (
        value.get("id") != image_id
        or value.get("os") != "linux"
        or value.get("architecture") != "amd64"
        or value.get("user") != "10001:10001"
        or value.get("cmd") != ["/app/simplestChat"]
        or value.get("entrypoint") not in (None, [])
        or not isinstance(labels, dict)
        or labels.get("org.opencontainers.image.revision") != revision
    ):
        raise BuildError(
            "Selected image must match this clean revision and the production runtime identity"
        )
    rootfs = value.get("rootfs")
    layers = (
        rootfs.get("Layers")
        if isinstance(rootfs, dict) and rootfs.get("Type") == "layers"
        else None
    )
    if (
        not isinstance(layers, list)
        or not 0 < len(layers) <= MAX_IMAGE_LAYERS
        or any(
            not isinstance(layer, str) or not re.fullmatch(r"sha256:[a-f0-9]{64}", layer)
            for layer in layers
        )
    ):
        raise BuildError("Selected image must expose bounded canonical filesystem layer identities")
    _, existing = runner.run(
        [*docker, "image", "ls", "--quiet", "--no-trunc", "--filter", f"reference={tag}"],
        cwd=root,
        env=env,
    )
    if existing and existing != image_id:
        raise BuildError(
            "The release tag already selects a different image; retained images are not overwritten"
        )
    if not existing:
        _ = runner.run([*docker, "image", "tag", image_id, tag], cwd=root, env=env)
    verify_selected_image(runner, docker, env, root, tag, image_id)
    return [string_value(layer) for layer in layers]


def verify_selected_image(  # noqa: PLR0913, PLR0917 -- Explicit boundary options preserve the subprocess contract.
    runner: RunnerProtocol,
    docker: list[str],
    env: dict[str, str],
    root: Path,
    tag: str,
    image_id: str,
) -> None:
    """Refuse export if the retained tag no longer names the selected image."""
    _, selected = runner.run(
        [*docker, "image", "inspect", "--format", "{{.Id}}", tag], cwd=root, env=env
    )
    if selected != image_id:
        raise BuildError("Release image selection changed during export")


def unpack_source(archive: Path, destination: Path) -> None:
    """Restore Git modes independently of the caller's private umask."""
    destination.mkdir(mode=0o755)
    destination.chmod(0o755)
    names: set[str] = set()
    with tarfile.open(archive, "r:") as contents:
        for member in contents:
            name = PurePosixPath(member.name)
            if (
                name.is_absolute()
                or ".." in name.parts
                or str(name) in names
                or str(name) in ("", ".")
            ):
                raise BuildError("Invalid source archive path")
            names.add(str(name))
            target = destination / name
            if member.isdir():
                target.mkdir(mode=0o755)
                target.chmod(0o755)
            elif member.isfile():
                source = contents.extractfile(member)
                if source is None:
                    raise BuildError("Source archive file content is unavailable")
                with source, target.open("xb") as output:
                    shutil.copyfileobj(source, output)
                target.chmod(0o755 if member.mode & 0o111 else 0o644)
            else:
                raise BuildError("Source archive links and special files are unsupported")


def migration_checksums(context: Path) -> dict[str, str]:
    """Collect ordered migration checksums from the archived revision."""
    migrations: dict[str, str] = {}
    for path in sorted((context / "migrations").glob("*.sql")):
        match = re.fullmatch(r"([0-9]+)_[A-Za-z0-9_]+\.sql", path.name)
        if match is None or not path.is_file() or path.is_symlink():
            raise BuildError("Invalid migration filename")
        version = str(int(match[1]))
        if version == "0" or int(version) > 2**63 - 1 or version in migrations:
            raise BuildError("Invalid or duplicate migration version")
        migrations[version] = hashlib.sha384(path.read_bytes()).hexdigest()
    if not 0 < len(migrations) <= MAX_MIGRATIONS:
        raise BuildError("Source archive must contain 1–1000 migrations")  # noqa: RUF001 -- Preserve the existing operational error text.
    return migrations


def build_release(  # noqa: C901, PLR0915 -- Keep ordered transaction checks together.
    output_value: str | Path,
    timeout: object,
    *,
    root: Path = ROOT,
    runner: RunnerProtocol | None = None,
    image_id: object = None,
) -> Path:
    """Build or export one pinned production image with retained failure evidence."""
    if type(timeout) is not int or not MIN_BUILD_SECONDS <= timeout <= MAX_BUILD_SECONDS:
        raise BuildError("Build timeout must be between 60 and 7200 seconds")
    if image_id is not None and (
        not isinstance(image_id, str) or not re.fullmatch(r"sha256:[a-f0-9]{64}", image_id)
    ):
        raise BuildError("Export requires an exact sha256 image ID, never a mutable tag")
    runner = runner or Runner()
    output = validate_output(output_value, root, runner)
    revision = clean_revision(runner, root)
    output.mkdir(mode=0o700)
    runner.output = output
    started = timestamp()
    passed = False
    error_text = None
    try:
        with (
            benchmark_guard(),
            tempfile.TemporaryDirectory(prefix="simplestchat-release.") as temporary,
        ):
            docker, env = docker_preflight(runner, root, require_builder=image_id is None)
            temporary_path = Path(temporary)
            source_archive = temporary_path / "source.tar"
            _ = runner.run(
                ["git", "archive", "--format=tar", f"--output={source_archive}", revision],
                cwd=root,
                capture=False,
            )
            context = temporary_path / "source"
            unpack_source(source_archive, context)
            migrations = migration_checksums(context)
            inputs: JsonObject = {
                name: ARTIFACT.sha256_file(context / name)
                for name in (
                    "Dockerfile",
                    ".dockerignore",
                    "Cargo.lock",
                    "web/package-lock.json",
                    "build/pip-constraints.txt",
                )
            }
            write_json(output / "source.json", {"revision": revision, "inputsSha256": inputs})
            tag = f"simplestchat-release/production:{revision}"
            deadline = time.monotonic() + timeout
            selected_layers = None
            if image_id is None:
                _ = runner.run(
                    [
                        *docker,
                        "buildx",
                        "build",
                        "--builder",
                        "default",
                        "--platform",
                        "linux/amd64",
                        "--pull",
                        "--progress",
                        "plain",
                        "--target",
                        "production",
                        "--provenance=false",
                        "--sbom=false",
                        "--build-arg",
                        f"SOURCE_REVISION={revision}",
                        "--label",
                        f"org.opencontainers.image.revision={revision}",
                        "--tag",
                        tag,
                        "--load",
                        str(context),
                    ],
                    cwd=context,
                    env=env,
                    timeout=timeout,
                    capture=False,
                )
            else:
                selected_layers = select_existing_image(
                    runner, docker, env, root, image_id, revision, tag
                )
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise BuildError("Build/export deadline expired before image export")  # noqa: TRY301 -- Record this validation failure in the same transaction evidence.
            _ = runner.run(
                [
                    *docker,
                    "image",
                    "save",
                    "--platform",
                    "linux/amd64",
                    "--output",
                    str(output / "image.tar"),
                    tag,
                ],
                cwd=root,
                env=env,
                timeout=min(remaining, 300),
                capture=False,
            )
            if image_id is not None:
                verify_selected_image(runner, docker, env, root, tag, image_id)
            if clean_revision(runner, root) != revision:
                raise BuildError("Checkout revision changed during release build")  # noqa: TRY301 -- Record this validation failure in the same transaction evidence.
            archive = output / "image.tar"
            manifest: ARTIFACT.Manifest = {
                "schemaVersion": 1,
                "revision": revision,
                "platform": "linux/amd64",
                "archiveSha256": ARTIFACT.sha256_file(archive),
                "imageTag": tag,
                "migrations": migrations,
                "createdAt": timestamp(),
            }
            candidate = output / "release.candidate.json"
            write_json(candidate, manifest)
            configuration = ARTIFACT.verify_archive(archive, ARTIFACT.validate_manifest(candidate))
            if image_id is not None:
                # Diff IDs are portable even when local image IDs differ across
                # Docker image stores. Bind exported filesystem metadata to the
                # selected image, in addition to validating its runtime identity.
                rootfs = configuration.get("rootfs")
                if (
                    not isinstance(rootfs, dict)
                    or rootfs.get("type") != "layers"
                    or rootfs.get("diff_ids") != selected_layers
                ):
                    raise BuildError(  # noqa: TRY301 -- Record this validation failure in the same transaction evidence.
                        "Exported filesystem layers differ from the selected immutable image"
                    )
            os.link(candidate, output / "release.json")
            passed = True
    except BaseException as error:
        error_text = str(error) or type(error).__name__
        raise
    finally:
        outcome: JsonObject = {
            "schemaVersion": 1,
            "revision": revision,
            "startedAt": started,
            "finishedAt": timestamp(),
            "passed": passed,
            "error": error_text,
        }
        if image_id is not None:
            outcome["exportedImageId"] = image_id
        write_json(output / "outcome.json", outcome)
        print(f"Release evidence: {output} (passed={str(passed).lower()})", flush=True)  # noqa: T201 -- Intentional CLI status output.
    return output


@dataclass
class BuildOptions(argparse.Namespace):
    """Validated parser destination types, without dynamic attribute access."""

    output: str = ""
    timeout_seconds: int = 3600
    image_id: str | None = None
    seen: set[str] = field(default_factory=set)


class Once(argparse.Action):
    """Reject duplicate build selectors before output creation."""

    @override
    def __call__(
        self,
        parser: argparse.ArgumentParser,
        namespace: argparse.Namespace,
        values: object,
        option_string: str | None = None,
    ) -> None:
        if not isinstance(namespace, BuildOptions):
            parser.error("invalid parser namespace")
        if self.dest in namespace.seen:
            parser.error(f"duplicate option: {option_string}")
        namespace.seen.add(self.dest)
        setattr(namespace, self.dest, values)


def main() -> int:
    """Run the CLI transaction and return a redacted success or failure status."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument(
        "--output",
        required=True,
        action=Once,
        help=(
            "Fresh directory with an existing parent; "
            + "use an ignored results/ child or a path outside the checkout"
        ),
    )
    _ = parser.add_argument(
        "--timeout-seconds",
        type=int,
        default=3600,
        action=Once,
        help="Build/export deadline, 60-7200 seconds (default: 3600)",
    )
    _ = parser.add_argument(
        "--image-id",
        action=Once,
        help="Export this exact existing sha256 image ID without building or pulling",
    )
    args = parser.parse_args(namespace=BuildOptions())
    _ = os.umask(0o077)

    def interrupted(signum: int, _frame: FrameType | None) -> None:
        raise BuildError(f"Interrupted by signal {signum}")

    _ = signal.signal(signal.SIGTERM, interrupted)
    try:
        _ = build_release(args.output, args.timeout_seconds, image_id=args.image_id)
    except (BuildError, ARTIFACT.ArtifactError, OSError, ValueError, KeyboardInterrupt) as error:
        print(f"Release build failed: {error}", file=sys.stderr)  # noqa: T201 -- Intentional CLI status output.
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
