"""Stage a trusted prebuilt release or replace only the public application.

No builds, pulls, migrations, account seeding, database restoration, or host
maintenance occur here. Persistent evidence and a workload lock make partial
updates visible. A failed candidate gets one bounded image/config rollback;
successful recovery never converts a failed release into a passed release.
"""

import argparse
import fcntl
import json
import os
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
import uuid
from collections.abc import Generator, Sequence
from contextlib import contextmanager
from copy import deepcopy
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path
from types import FrameType
from typing import NoReturn, Protocol, TypedDict, Unpack

from release_artifact import ArtifactError, Manifest, sha256_file, validate_manifest, verify_archive
from release_json import JsonObject, JsonValue, decode_json, object_value, string_value

ROOT = Path("/srv/simplestchat-public")
CONFIG = Path("/etc/simplestchat-public")
WORK = Path("/run/simplestchat-bench")
ENV = {"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LC_ALL": "C"}
DOCKER = ["/usr/bin/docker", "--host", "unix:///var/run/docker.sock"]
SELECTION = ("compose.public.yml", "app.env", "images.json")
ID = re.compile(r"sha256:[a-f0-9]{64}")
MAX_INSPECTION_BYTES = 2 * 1024 * 1024
LEDGER_COLUMNS = 3
IMAGE_SELECTIONS = 2
CONTAINER_FORMAT = (
    '{"id":{{json .Id}},"image":{{json .Image}},"state":{{json .State}},'
    '"restarts":{{.RestartCount}},'
    '"configHash":{{json (index .Config.Labels "com.docker.compose.config-hash")}}}'
)
IMAGE_FORMAT = (
    '{"id":{{json .Id}},"os":{{json .Os}},"architecture":{{json .Architecture}},'
    '"user":{{json .Config.User}},"labels":{{json .Config.Labels}},'
    '"cmd":{{json .Config.Cmd}},"entrypoint":{{json (index .Config "Entrypoint")}}}'
)
LEDGER_QUERY = (
    "SELECT version, success, encode(checksum, 'hex') FROM public._sqlx_migrations ORDER BY version"
)


class CommandOptions(TypedDict, total=False):
    """Optional command controls shared by real runners and typed test doubles."""

    timeout: float
    input_data: bytes | None
    input_path: Path | None
    output_path: Path | None


class AttemptContext(Protocol):
    """Evidence ownership needed to update a persistent operation journal."""

    @property
    def attempt(self) -> Path:
        """Return the existing private evidence directory."""
        ...


class RunnerProtocol(AttemptContext, Protocol):
    """Permit only bounded byte-producing commands and scoped container lookup."""

    def run(
        self,
        args: Sequence[str],
        *,
        timeout: float = 30,
        input_data: bytes | None = None,
        input_path: Path | None = None,
        output_path: Path | None = None,
    ) -> bytes:
        """Run a bounded command and retain its private evidence."""
        ...

    def docker(self, *args: str, **kwargs: Unpack[CommandOptions]) -> bytes:
        """Run a command against the fixed local Docker endpoint."""
        ...

    def compose(
        self,
        *args: str,
        filename: Path | None = None,
        envfile: Path | None = None,
        **kwargs: Unpack[CommandOptions],
    ) -> bytes:
        """Run a command against the selected public Compose project."""
        ...

    def container(self, service: str) -> JsonObject:
        """Inspect exactly one running project container."""
        ...


@dataclass(frozen=True, slots=True, kw_only=True)
class JournalContext:
    """Bind recovery journal writes to the original preparation directory."""

    attempt: Path


@dataclass(frozen=True, slots=True, kw_only=True)
class ReleaseOptions:
    """Validated immutable command selection for a single release operation."""

    action: str
    revision: str
    quiet_seconds: int = 0


class _ArgumentValues(argparse.Namespace):
    action: str = ""
    revision: str = ""
    quiet_seconds: int = 0


class ReleaseError(RuntimeError):
    """A bounded release operation failed; inspect its private evidence."""


@dataclass(frozen=True, slots=True, kw_only=True)
class TurnConfiguration:
    """The only supported runtime change: enable the separately verified local relay."""

    domain: str
    secret: str = field(repr=False)

    def environment(self) -> dict[str, str]:
        """Return fixed TURN settings after rejecting dotenv or URL injection."""
        require(
            re.fullmatch(r"(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,63}", self.domain)
            and re.fullmatch(r"[a-f0-9]{64}", self.secret),
            "Invalid managed TURN configuration",
        )
        return {
            "TURN_URLS": (
                f"turn:{self.domain}:3478?transport=udp,"
                f"turn:{self.domain}:3478?transport=tcp,"
                f"turns:{self.domain}:5349?transport=tcp"
            ),
            "TURN_SECRET": self.secret,
            "TURN_TTL": "86400",
        }


def timestamp() -> str:
    """Return a UTC timestamp suitable for durable JSON evidence."""
    return datetime.now(UTC).strftime("%Y-%m-%dT%H:%M:%SZ")


def require(condition: object, message: str) -> None:
    """Reject an unmet invariant without substituting another deployment action."""
    if not condition:
        raise ReleaseError(message)


def protected(
    path: Path,
    *,
    directory: bool = False,
    modes: tuple[int, ...] = (0o600,),
    limit: int | None = None,
) -> None:
    """Require an ordinary root-owned path with approved permissions and size."""
    metadata = path.lstat()
    require(
        metadata.st_uid == 0 and stat.S_IMODE(metadata.st_mode) in modes,
        f"Unexpected ownership or permissions: {path}",
    )
    require(
        stat.S_ISDIR(metadata.st_mode) if directory else stat.S_ISREG(metadata.st_mode),
        f"Unexpected file type: {path}",
    )
    if limit is not None:
        require(metadata.st_size <= limit, f"Oversized configuration: {path}")


def atomic(path: Path, data: JsonValue | bytes) -> None:
    """Replace one private regular file durably; the journal covers the set."""
    if not isinstance(data, (str, bytes)):
        data = json.dumps(data, indent=2) + "\n"
    if isinstance(data, str):
        data = data.encode()
    descriptor, temporary = tempfile.mkstemp(prefix=".release-", dir=path.parent)
    with os.fdopen(descriptor, "wb") as output:
        _ = output.write(data)
        output.flush()
        os.fsync(output.fileno())
    _ = Path(temporary).replace(path)
    directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


class Runner:
    """Bound commands and retain output privately without printing secrets."""

    def __init__(self, attempt: Path) -> None:
        """Retain each command's output beneath the caller-owned attempt."""
        self.attempt: Path = attempt
        self.number: int = 0

    def run(
        self,
        args: Sequence[str],
        *,
        timeout: float = 30,
        input_data: bytes | None = None,
        input_path: Path | None = None,
        output_path: Path | None = None,
    ) -> bytes:
        """Execute explicit argv with bounded cleanup and private immutable output."""
        self.number += 1
        prefix = self.attempt / f"{self.number:03d}"
        target = output_path or prefix.with_suffix(".stdout")
        source = input_path.open("rb") if input_path else None
        process: subprocess.Popen[bytes] | None = None
        try:
            with target.open("xb") as output, prefix.with_suffix(".stderr").open("xb") as error:
                process = subprocess.Popen(  # noqa: S603 - fixed local argv; no shell or inherited environment.
                    args,
                    stdin=source
                    or (subprocess.PIPE if input_data is not None else subprocess.DEVNULL),
                    stdout=output,
                    stderr=error,
                    env=ENV,
                    start_new_session=True,
                )
                _ = process.communicate(input_data, timeout=timeout)
        finally:
            if source:
                source.close()
            if process is not None and process.poll() is None:
                self.stop(process)
        if process is None:
            message = "Command failed before creating an owned process"
            raise ReleaseError(message)
        require(
            process.returncode == 0, f"Command {self.number:03d} failed; inspect private output"
        )
        if output_path:
            return b""
        require(
            target.stat().st_size <= MAX_INSPECTION_BYTES,
            "Command inspection output exceeded its bound",
        )
        return target.read_bytes()

    @staticmethod
    def stop(process: subprocess.Popen[bytes]) -> None:
        """Terminate only the owned subprocess group and observe its exit."""

        def send(signum: signal.Signals) -> None:
            try:
                os.killpg(process.pid, signum)
            except (ProcessLookupError, PermissionError):
                process.send_signal(signum)

        send(signal.SIGTERM)
        try:
            _ = process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            send(signal.SIGKILL)
            _ = process.wait(timeout=5)

    def docker(self, *args: str, **kwargs: Unpack[CommandOptions]) -> bytes:
        """Use the fixed local Unix socket without inherited Docker configuration."""
        return self.run([*DOCKER, *args], **kwargs)

    def compose(
        self,
        *args: str,
        filename: Path | None = None,
        envfile: Path | None = None,
        **kwargs: Unpack[CommandOptions],
    ) -> bytes:
        """Keep every Compose command bound to the prepared public project."""
        return self.docker(
            "compose",
            "--project-name",
            "simplestchat-public",
            "--project-directory",
            str(CONFIG),
            "--env-file",
            str(envfile or CONFIG / "app.env"),
            "-f",
            str(filename or CONFIG / "compose.public.yml"),
            *args,
            **kwargs,
        )

    def container(self, service: str) -> JsonObject:
        """Inspect the single running container belonging to a named service."""
        value = self.compose("ps", "--status", "running", "--quiet", service).decode().strip()
        require(
            re.fullmatch(r"[a-f0-9]{64}", value),
            f"Exactly one running {service} container is required",
        )
        result = object_value(
            decode_json(
                self.docker(
                    "inspect",
                    "--format",
                    CONTAINER_FORMAT,
                    value,
                )
            )
        )
        state = object_value(result["state"])
        require(state["Running"] and not state["OOMKilled"], f"{service} is not running normally")
        return result


def stable_container(value: JsonObject) -> tuple[JsonValue, JsonValue, JsonValue, JsonValue]:
    """Select fields that prove a retained dependency was not restarted."""
    return value["id"], value["image"], object_value(value["state"])["StartedAt"], value["restarts"]


def boot_id() -> str:
    """Read and validate the current Linux boot identity."""
    value = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
    require(
        re.fullmatch(r"[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}", value),
        "Invalid host boot identity",
    )
    return value


@contextmanager
def workload_lock(*, after_reboot: bool = False, cancel_reboot: bool = False) -> Generator[None]:
    """Serialize work and reject any unfinished persistent ownership journal."""
    require(not (after_reboot and cancel_reboot), "Choose one reboot recovery action")
    protected(ROOT, directory=True, modes=(0o700,))
    protected(CONFIG, directory=True, modes=(0o700,))
    WORK.mkdir(mode=0o700, exist_ok=True)
    protected(WORK, directory=True, modes=(0o700,))
    try:
        descriptor = os.open(WORK / "workload.lock", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        os.close(descriptor)
    except FileExistsError:
        pass
    protected(WORK / "workload.lock")
    with (WORK / "workload.lock").open("r+") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        current = WORK / "current.json"
        if current.exists() or current.is_symlink():
            protected(current, limit=16384)
            record = object_value(decode_json(current.read_text()))
            require(
                type(record.get("schemaVersion")) is int
                and record["schemaVersion"] == 1
                and record.get("finalized") is True,
                "Resolve unfinished benchmark cleanup before releasing",
            )
        state = ROOT / "release-state.json"
        if state.exists() or state.is_symlink():
            protected(state, limit=16384)
            record = object_value(decode_json(state.read_text()))
            selected_boot = record.get("bootId")
            prepared = (
                record.get("phase") == "await_reboot"
                and isinstance(selected_boot, str)
                and re.fullmatch(
                    r"[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}", selected_boot
                )
            )
            resumed_boot = after_reboot and prepared and record["bootId"] != boot_id()
            cancelled_boot = cancel_reboot and prepared and record["bootId"] == boot_id()
            require(
                type(record.get("schemaVersion")) is int
                and record["schemaVersion"] == 1
                and (record.get("finalized") is True or resumed_boot or cancelled_boot),
                "Inspect and recover the unfinished release before starting another",
            )
        yield


def image_identity(runner: RunnerProtocol, selector: str, revision: str) -> str:
    """Require the selected production runtime to match its reviewed revision."""
    # Docker may omit an unset optional Entrypoint from the image Config map.
    # Keep required fields strict; index only this optional value to obtain null.
    value = object_value(
        decode_json(
            runner.docker(
                "image",
                "inspect",
                "--format",
                IMAGE_FORMAT,
                selector,
            )
        )
    )
    identifier = string_value(value["id"])
    require(ID.fullmatch(identifier), "Missing local content-addressed image ID")
    require(
        value["os"] == "linux"
        and value["architecture"] == "amd64"
        and value["user"] == "10001:10001",
        "Unexpected image platform or runtime user",
    )
    require(
        object_value(value["labels"]).get("org.opencontainers.image.revision") == revision,
        "Image revision mismatch",
    )
    require(
        value["cmd"] == ["/app/simplestChat"] and value["entrypoint"] in (None, []),
        "Unexpected image entrypoint",
    )
    return identifier


def packaged_migrations(runner: RunnerProtocol, image: str) -> dict[str, str]:
    """Execute only the fixed checksum command in a bounded, isolated container."""
    name = "scpub-release-validate-" + uuid.uuid4().hex
    journal(runner, finalized=False, phase="validate_image")
    atomic(runner.attempt / "validation-name.txt", name)
    # Keep timeout below PID 1: some packaged versions reject an init-parented
    # child as orphaned. Docker's init also forwards signals and reaps children.
    container = (
        runner.docker(
            "create",
            "--name",
            name,
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
            # The local driver's default compression requires more than one file.
            "--log-opt",
            "max-size=1m",
            "--log-opt",
            "max-file=1",
            "--log-opt",
            "compress=false",
            "--entrypoint",
            "/usr/bin/timeout",
            image,
            "--signal=TERM",
            "--kill-after=2s",
            "10s",
            "/bin/sh",
            "-c",
            'for file in /app/migrations/*.sql; do sha384sum "$file" || exit; done',
        )
        .decode()
        .strip()
    )
    require(re.fullmatch(r"[a-f0-9]{64}", container), "Uncertain validation container creation")
    _ = runner.docker("start", container)
    require(
        runner.docker("wait", container, timeout=20).strip() == b"0",
        "Packaged migration validation failed",
    )
    lines = runner.docker("logs", container).decode().splitlines()
    result: dict[str, str] = {}
    for line in lines:
        match = re.fullmatch(r"([a-f0-9]{96})  /app/migrations/([0-9]+)_[A-Za-z0-9_]+\.sql", line)
        if match is None:
            message = "Unexpected packaged migration output"
            raise ReleaseError(message)
        version = str(int(match[2]))
        require(version not in result, "Duplicate packaged migration version")
        result[version] = match[1]
    require(result, "No packaged migrations")
    _ = runner.docker("rm", container)
    journal(runner, finalized=True, phase="image_validated")
    return result


def journal(runner: AttemptContext, finalized: bool, phase: str) -> None:  # noqa: FBT001 - preserve the established positional helper API.
    """Durably record the operation's current ownership and settlement state."""
    atomic(
        ROOT / "release-state.json",
        {
            "schemaVersion": 1,
            "attempt": str(runner.attempt),
            "finalized": finalized,
            "phase": phase,
        },
    )


def stage(runner: RunnerProtocol, directory: Path, manifest: Manifest) -> JsonObject:
    """Validate and import one immutable artifact without stopping public services."""
    _ = verify_archive(directory / "image.tar", manifest)
    selected = directory / "staged.json"
    if selected.exists() or selected.is_symlink():
        protected(selected, limit=16384)
        prior = object_value(decode_json(selected.read_text()))
        require(
            prior["manifestSha256"] == sha256_file(directory / "release.json"),
            "Staged release changed",
        )
        require(
            image_identity(runner, string_value(prior["serverImage"]), manifest["revision"])
            == prior["serverImage"],
            "Staged image missing",
        )
        return prior
    require(
        shutil.disk_usage(ROOT).free > (directory / "image.tar").stat().st_size * 2 + 1024**3,
        "Insufficient free space to import image with safe headroom",
    )
    journal(runner, finalized=False, phase="import_image")
    _ = runner.docker("image", "load", "--input", str(directory / "image.tar"), timeout=300)
    image = image_identity(runner, manifest["imageTag"], manifest["revision"])
    require(
        packaged_migrations(runner, image) == manifest["migrations"],
        "Archive migration manifest differs from image",
    )
    record: JsonObject = {
        "schemaVersion": 1,
        "revision": manifest["revision"],
        "serverImage": image,
        "manifestSha256": sha256_file(directory / "release.json"),
        "stagedAt": timestamp(),
    }
    atomic(selected, record)
    journal(runner, finalized=True, phase="staged")
    return record


def ledger(runner: RunnerProtocol, database: str) -> dict[str, str]:
    """Read successful migration versions and exact checksums from the live database."""
    lines = (
        runner.docker(
            "exec",
            "--user",
            "999:999",
            database,
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
            "--field-separator",
            " ",
            "--command",
            LEDGER_QUERY,
        )
        .decode()
        .splitlines()
    )
    result: dict[str, str] = {}
    for line in lines:
        fields = line.split()
        require(
            len(fields) == LEDGER_COLUMNS
            and re.fullmatch(r"[1-9][0-9]*", fields[0])
            and fields[1] == "t"
            and re.fullmatch(r"[a-f0-9]{96}", fields[2])
            and fields[0] not in result,
            "Invalid migration ledger",
        )
        result[fields[0]] = fields[2]
    require(result, "Empty migration ledger")
    return result


QUIET_POLL_SECONDS = 5.0
MAX_QUIET_SECONDS = 600


def metrics_token() -> str | None:
    """Return the app's metrics bearer token from its private environment file, if set."""
    for line in (CONFIG / "app.env").read_text().splitlines():
        if line.startswith("METRICS_TOKEN="):
            token = line.partition("=")[2].strip()
            return token if token and re.fullmatch(r"[A-Za-z0-9._~-]{32,512}", token) else None
    return None


def active_rooms(runner: RunnerProtocol, curl_config: Path) -> int | None:
    """`simplestchat_rooms_active` from the local metrics endpoint; None when unavailable."""
    try:
        body = runner.run(
            [
                "/usr/bin/curl",
                "--disable",
                "--noproxy",
                "*",
                "--proto",
                "=http",
                "--fail",
                "--silent",
                "--show-error",
                "--max-time",
                "2",
                "--config",
                str(curl_config),
                "http://127.0.0.1:3000/metrics",
            ],
            timeout=5,
        ).decode()
    except (ReleaseError, UnicodeDecodeError):
        return None
    for line in body.splitlines():
        if line.startswith("simplestchat_rooms_active "):
            try:
                return int(line.split()[1])
            except (IndexError, ValueError):
                return None
    return None


def quiet(runner: RunnerProtocol, report: JsonObject, seconds: int) -> None:
    """Wait, bounded, for zero active rooms so the replacement interrupts no call.

    The wait needs the metrics token from app.env; without it, or when the
    endpoint is unreadable, the release proceeds and records that it could
    not tell. A deadline reached with rooms still active also proceeds: the
    operator chose the bound, and the report shows what was interrupted.
    """
    report["quietWaitRequestedSeconds"] = seconds
    token = metrics_token() if seconds > 0 else None
    if token is None:
        report["quietWaitSeconds"] = 0
        report["roomsActiveAtReplacement"] = None
        return
    curl_config = runner.attempt / "metrics-curl.config"
    _ = curl_config.write_text(f'header = "Authorization: Bearer {token}"\n')
    curl_config.chmod(0o600)
    started = time.monotonic()
    deadline = started + seconds
    rooms = active_rooms(runner, curl_config)
    while rooms is not None and rooms > 0 and time.monotonic() < deadline:
        time.sleep(min(QUIET_POLL_SECONDS, max(0.0, deadline - time.monotonic())))
        rooms = active_rooms(runner, curl_config)
    report["quietWaitSeconds"] = round(time.monotonic() - started, 3)
    report["roomsActiveAtReplacement"] = rooms


def ready(runner: RunnerProtocol, *, origin: str | None = None, seconds: float = 30) -> None:
    """Require bounded readiness on loopback or trusted public HTTPS."""
    deadline = time.monotonic() + seconds
    url = (origin or "http://127.0.0.1:3000") + "/ready"
    while True:
        try:
            value = object_value(
                decode_json(
                    runner.run(
                        [
                            "/usr/bin/curl",
                            "--disable",
                            "--noproxy",
                            "*",
                            "--proto",
                            "=https" if origin else "=http",
                            "--fail",
                            "--silent",
                            "--show-error",
                            "--max-time",
                            "2",
                            url,
                        ],
                        timeout=5,
                    )
                )
            )
            require(value.get("status") == "ready", "Application is not ready")
        except (ReleaseError, ValueError):
            if time.monotonic() >= deadline:
                message = "Application readiness deadline exceeded"
                raise ReleaseError(message) from None
            time.sleep(0.5)
        else:
            return


def candidate_selection(
    runner: RunnerProtocol,
    new_image: str,
    old: JsonObject,
    turn: TurnConfiguration | None = None,
) -> tuple[bytes, bytes]:
    """Allow image selection and, explicitly, enabling the managed TURN relay."""
    compose = (CONFIG / "compose.public.yml").read_text()
    needle = f'image: "{old["serverImage"]}"'
    require(
        compose.count(needle) == IMAGE_SELECTIONS,
        "Expected only app and migration image selections; reapply reviewed configuration",
    )
    environment = (CONFIG / "app.env").read_text()
    needle_env = f"SIMPLESTCHAT_IMAGE={old['serverImage']}"
    require(
        environment.splitlines().count(needle_env) == 1,
        "Current image environment differs from selection",
    )
    preview = runner.attempt / "candidate-compose.yml"
    preview_env = runner.attempt / "candidate.env"
    atomic(preview, compose.replace(needle, f'image: "{new_image}"'))
    atomic(
        preview_env,
        "\n".join(
            f"SIMPLESTCHAT_IMAGE={new_image}" if line == needle_env else line
            for line in environment.splitlines()
        )
        + "\n",
    )
    selected_compose = preview.read_bytes()
    if turn is not None:
        values = turn.environment()
        require(
            not any(line.partition("=")[0] in values for line in environment.splitlines()),
            "TURN is already configured; rotation requires separate maintenance",
        )
        atomic(
            preview_env,
            preview_env.read_bytes()
            + "".join(f"{key}={value}\n" for key, value in values.items()).encode(),
        )
        # Override this service's env_file only for validation; the installed
        # Compose file continues to reference its ordinary protected app.env.
        require(compose.count("- ./app.env") == 1, "Unexpected application env_file selection")
        atomic(preview, selected_compose.replace(b"- ./app.env", f'- "{preview_env}"'.encode()))
    before = object_value(
        decode_json(runner.compose("--profile", "maintenance", "config", "--format", "json"))
    )
    after = object_value(
        decode_json(
            runner.compose(
                "--profile",
                "maintenance",
                "config",
                "--format",
                "json",
                filename=preview,
                envfile=preview_env,
            )
        )
    )
    expected = deepcopy(before)
    for service in ("simplestchat", "migrate"):
        object_value(object_value(expected["services"])[service])["image"] = new_image
    if turn is not None:
        expected_environment = object_value(
            object_value(object_value(expected["services"])["simplestchat"])["environment"]
        )
        expected_environment.update(turn.environment())
        expected_environment["SIMPLESTCHAT_IMAGE"] = new_image
    # app.env is also a service env_file; preview overrides only interpolation,
    # so its image metadata is changed when the real file is installed below.
    require(after == expected, "Candidate changes configuration beyond the reviewed selection")
    application = object_value(object_value(before["services"])["simplestchat"])
    require(
        object_value(application["environment"])["RUN_MIGRATIONS"] == "false",
        "Runtime migrations must remain disabled",
    )
    return selected_compose, preview_env.read_bytes()


def deploy(  # noqa: PLR0913, PLR0915 - explicit opt-in settings; keep replacement and bounded rollback together.
    runner: RunnerProtocol,
    manifest: Manifest,
    staged: JsonObject,
    report: JsonObject,
    quiet_seconds: int = 0,
    *,
    turn: TurnConfiguration | None = None,
) -> None:
    """Replace only the app, preserving backup evidence and one bounded rollback."""
    for filename in SELECTION:
        protected(CONFIG / filename, limit=1024 * 1024)
    old = object_value(decode_json((CONFIG / "images.json").read_text()))
    old_image = string_value(old["serverImage"])
    old_revision = string_value(old["revision"])
    require(
        ID.fullmatch(old_image) and re.fullmatch(r"[a-f0-9]{40}", old_revision),
        "Invalid deployed identity",
    )
    _ = image_identity(runner, old_image, old_revision)
    new_image = image_identity(runner, string_value(staged["serverImage"]), manifest["revision"])
    require(old["serverImage"] != new_image or turn is not None, "This image is already selected")
    app, database, proxy = (
        runner.container(service) for service in ("simplestchat", "postgres", "caddy")
    )
    for service, container in (("simplestchat", app), ("postgres", database), ("caddy", proxy)):
        fields = runner.compose("config", "--hash", service).decode().split()
        require(
            fields == [service, container["configHash"]],
            "Running configuration differs from disk; use reviewed maintenance",
        )
    require(app["image"] == old["serverImage"], "Running app differs from selected image")
    database_state = object_value(database["state"])
    require(
        object_value(database_state.get("Health", {})).get("Status") == "healthy",
        "Database must be healthy",
    )
    require(
        not runner.compose("--profile", "maintenance", "ps", "--all", "--quiet", "migrate").strip(),
        "Inspect retained migration container first",
    )
    ready(runner, seconds=3)
    database_id = string_value(database["id"])
    require(
        ledger(runner, database_id) == manifest["migrations"],
        "Schema changes require the explicit maintenance deployment",
    )
    require(
        packaged_migrations(runner, new_image) == manifest["migrations"],
        "Candidate migration mismatch",
    )
    preview, preview_env = candidate_selection(runner, new_image, old, turn)
    config = object_value(decode_json(runner.compose("config", "--format", "json")))
    environment = object_value(
        object_value(object_value(config["services"])["simplestchat"])["environment"]
    )
    origin = string_value(environment["WEBAUTHN_ORIGIN"])
    require(re.fullmatch(r"https://[a-z0-9.-]+", origin), "Unexpected public origin")
    ready(runner, origin=origin, seconds=3)
    quiet(runner, report, quiet_seconds)
    backup = runner.attempt / "database-before.dump"
    require(shutil.disk_usage(ROOT).free > 1024**3, "Insufficient backup headroom")
    journal(runner, finalized=False, phase="live_backup")
    _ = runner.docker(
        "exec",
        "--user",
        "999:999",
        database_id,
        "timeout",
        "--signal=TERM",
        "--kill-after=2s",
        "45s",
        "pg_dump",
        "--host",
        "/run/simplestchat-postgres",
        "--username",
        "postgres",
        "--dbname",
        "simplestchat",
        "--format",
        "custom",
        timeout=60,
        output_path=backup,
    )
    require(backup.stat().st_size > 0, "Empty live database backup")
    _ = runner.docker(
        "exec",
        "--interactive",
        "--user",
        "999:999",
        database_id,
        "timeout",
        "--signal=TERM",
        "--kill-after=2s",
        "15s",
        "pg_restore",
        "--list",
        input_path=backup,
    )
    report["backupSha256"] = sha256_file(backup)
    journal(runner, finalized=True, phase="backed_up")
    for filename in SELECTION:
        _ = shutil.copyfile(CONFIG / filename, runner.attempt / ("before-" + filename))
    journal(runner, finalized=False, phase="replace_application")
    replaced = False
    try:
        report["phase"] = "replace_application"
        report["interruptionStartedAt"] = timestamp()
        replaced = True
        _ = runner.compose("stop", "--timeout", "30", "simplestchat", timeout=45)
        atomic(CONFIG / "compose.public.yml", preview)
        atomic(CONFIG / "app.env", preview_env)
        atomic(
            CONFIG / "images.json", dict(old, revision=manifest["revision"], serverImage=new_image)
        )
        _ = runner.compose(
            "up",
            "--detach",
            "--no-build",
            "--pull",
            "never",
            "--no-deps",
            "simplestchat",
            timeout=60,
        )
        require(
            runner.container("simplestchat")["image"] == new_image,
            "Replacement is not the staged image",
        )
        ready(runner)
        ready(runner, origin=origin)
        require(
            stable_container(runner.container("postgres")) == stable_container(database)
            and stable_container(runner.container("caddy")) == stable_container(proxy),
            "A retained dependency restarted during release",
        )
        report["interruptionFinishedAt"] = timestamp()
        report["phase"] = "complete"
        journal(runner, finalized=True, phase="complete")
    except BaseException:
        report["rollbackAttempted"] = replaced
        if replaced:
            try:
                failed = runner.compose("ps", "--all", "--quiet", "simplestchat").decode().strip()
                if re.fullmatch(r"[a-f0-9]{64}", failed):
                    _ = runner.docker(
                        "inspect",
                        "--format",
                        '{"image":{{json .Image}},"state":{{json .State}}}',
                        failed,
                    )
                _ = runner.compose("logs", "--no-color", "--tail", "100", "simplestchat")
            except (ReleaseError, subprocess.TimeoutExpired):
                pass
            try:
                _ = runner.compose("stop", "--timeout", "30", "simplestchat", timeout=45)
                for filename in SELECTION:
                    atomic(
                        CONFIG / filename, (runner.attempt / ("before-" + filename)).read_bytes()
                    )
                _ = runner.compose(
                    "up",
                    "--detach",
                    "--no-build",
                    "--pull",
                    "never",
                    "--no-deps",
                    "simplestchat",
                    timeout=60,
                )
                require(
                    runner.container("simplestchat")["image"] == old["serverImage"],
                    "Rollback image mismatch",
                )
                ready(runner)
                ready(runner, origin=origin)
                require(
                    stable_container(runner.container("postgres")) == stable_container(database)
                    and stable_container(runner.container("caddy")) == stable_container(proxy),
                    "Dependency continuity changed",
                )
                report["rollbackPassed"] = True
                journal(runner, finalized=True, phase="rolled_back")
            except BaseException:  # noqa: BLE001 - rollback failure must not mask the original failure.
                report["rollbackPassed"] = False
        raise


def main() -> None:
    """Run one explicit staging or deployment transaction and retain its outcome."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("action", choices=("stage", "deploy"))
    _ = parser.add_argument("revision")
    _ = parser.add_argument(
        "--quiet-seconds",
        type=int,
        default=0,
        help="wait up to this long for zero active rooms before replacing the app",
    )
    raw = parser.parse_args(namespace=_ArgumentValues())
    arguments = ReleaseOptions(
        action=raw.action, revision=raw.revision, quiet_seconds=raw.quiet_seconds
    )
    require(os.geteuid() == 0, "Run as root on the prepared public host")
    require(re.fullmatch(r"[a-f0-9]{40}", arguments.revision), "Use the exact release commit")
    require(
        0 <= arguments.quiet_seconds <= MAX_QUIET_SECONDS,
        f"--quiet-seconds must be between 0 and {MAX_QUIET_SECONDS}",
    )
    _ = os.umask(0o077)

    def interrupted(_signum: int, _frame: FrameType | None) -> NoReturn:
        message = "Release interrupted"
        raise ReleaseError(message)

    for signum in (signal.SIGTERM, signal.SIGINT):
        _ = signal.signal(signum, interrupted)
    with workload_lock():
        protected(ROOT / "releases", directory=True, modes=(0o700,))
        directory = ROOT / "releases" / arguments.revision
        protected(directory, directory=True, modes=(0o700,))
        for filename in ("release.json", "image.tar"):
            protected(directory / filename)
        manifest = validate_manifest(directory / "release.json")
        require(manifest["revision"] == arguments.revision, "Release directory identity differs")
        protected(ROOT / "results", directory=True, modes=(0o700,))
        attempt = Path(tempfile.mkdtemp(prefix="release.", dir=ROOT / "results"))
        report: JsonObject = {
            "action": arguments.action,
            "revision": arguments.revision,
            "startedAt": timestamp(),
            "passed": False,
            "phase": "stage",
        }
        try:
            runner = Runner(attempt)
            staged = stage(runner, directory, manifest)
            if arguments.action == "deploy":
                report["phase"] = "preflight"
                deploy(runner, manifest, staged, report, quiet_seconds=arguments.quiet_seconds)
            else:
                report["phase"] = "complete"
            report["passed"] = True
        except BaseException as error:
            report["failure"] = (
                str(error)
                if isinstance(error, (ReleaseError, ArtifactError))
                else type(error).__name__
            )
            raise
        finally:
            report["finishedAt"] = timestamp()
            atomic(attempt / "outcome.json", report)
            _ = sys.stdout.write(json.dumps({"evidence": str(attempt), **report}) + "\n")


def cli() -> int:
    """Preserve the standalone command's sanitized error and exit behavior."""
    try:
        main()
    except (
        ReleaseError,
        ArtifactError,
        OSError,
        ValueError,
        KeyError,
        subprocess.TimeoutExpired,
    ) as error:
        failure = type(error).__name__
        _ = sys.stderr.write(
            f"Release failed ({failure}); inspect private evidence before another attempt.\n"
        )
        return 1
    return 0
