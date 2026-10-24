"""Observe only authenticated Stage effects inside one marked disposable guest.

Native actor lifecycle and the separately inspected probe's isolation are distinct
facts. Candidate records are passive until the real process/finalizer verdict.
"""

from __future__ import annotations

import base64
import datetime
import fcntl
import hashlib
import json
import math
import os
import re
import signal
import stat
import subprocess
import sys
import tarfile
import time
from pathlib import Path
from typing import TYPE_CHECKING, cast, final

LIBRARY = Path("/usr/local/libexec/simplestchat-public")
ROOT = Path("/root/simplestchat-vm-fixture")
PUBLIC = Path("/srv/simplestchat-public")
WORK = Path("/run/simplestchat-bench")
MARKER = Path("/etc/simplestchat-vm-fixture")
INSTALLED = LIBRARY / "simplestchat-ops"
DOCKER = ("/usr/bin/docker", "--host", "unix:///var/run/docker.sock")
MAX_RECORD = 131072
MAX_STREAM = 2 * 1024**2
MAX_IMAGE = 2 * 1024**3
MINIMUM_NATIVE = 130
MINIMUM_BODY = 2 * MINIMUM_NATIVE + 40
MAX_MIGRATIONS = 10000
EVENT_HISTORY = 256
MAX_COMMANDS = 40
DIRECTORY_MODE = 0o700
PRIVATE_MODE = 0o600
PROBE_NANO_CPUS = 500000000
PROBE_PIDS = 32
FIRST_COMMANDS = 7
MAX_EVIDENCE_ATTEMPTS = 4
MAX_EVIDENCE_FILES = 145
EVIDENCE_ARGC = 3
ENV = {"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LC_ALL": "C", "HOME": str(ROOT)}
# These imports are the unchanged, root-staged trusted assertion dependencies.
if __name__ == "__main__":
    sys.path[:0] = [str(ROOT), str(LIBRARY)]
import security_vm_ops_guest as ops_guest  # noqa: E402

# isort: split
import bounded_process  # noqa: E402 -- Fixed root-staged trusted import boundary.
import release_artifact as artifact  # noqa: E402 -- Fixed root-staged trusted import boundary.
import runtime_profile  # noqa: E402 -- Fixed root-staged trusted import boundary.
from release_json import decode_json, object_value  # noqa: E402 -- Fixed trusted import boundary.

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping, Sequence
    from typing import BinaryIO

    from release_json import JsonObject


def require(value: object) -> None:
    """Expose only one fixed failure label, never private external values."""
    if not value:
        message = "stage_vm_guest_unproven"
        raise ValueError(message)


def record(data: bytes) -> dict[str, object]:
    """Decode one bounded duplicate-free whole JSON value, including pretty output."""
    require(0 < len(data) <= MAX_RECORD)
    return cast("dict[str, object]", object_value(decode_json(data)))


def exact(value: object, fields: set[str]) -> dict[str, object]:
    """Reject extra fields before projecting anything to a public summary."""
    require(isinstance(value, dict))
    result = cast("dict[str, object]", value)
    require(set(result) == fields)
    return result


def digest(value: object, count: int = 64) -> str:
    """Return only a complete fixed-format hexadecimal identity."""
    require(isinstance(value, str) and re.fullmatch("[a-f0-9]{" + str(count) + "}", value))
    return cast("str", value)


def integer(value: object, low: int, high: int) -> int:
    """Booleans and non-finite/coerced numeric values are not counters."""
    require(type(value) is int and low <= value <= high)
    return cast("int", value)


def request(data: bytes) -> dict[str, object]:
    """Accept only current original pins and a clipped remaining fixture duration."""
    value = exact(
        record(data),
        {
            "schemaVersion",
            "runId",
            "revision",
            "bodySeconds",
            "binary",
            "manifest",
            "archive",
            "guestSha256",
            "helperPins",
        },
    )
    require(type(value["schemaVersion"]) is int and value["schemaVersion"] == 1)
    _ = digest(value["runId"], 32)
    _ = digest(value["revision"], 40)
    _ = digest(value["guestSha256"])
    _ = integer(value["bodySeconds"], MINIMUM_BODY, 1640)
    for key, maximum in (
        ("binary", ops_guest.MAX_BINARY),
        ("manifest", 2**20),
        ("archive", MAX_IMAGE),
    ):
        item = exact(value[key], {"sha256", "bytes"})
        _ = digest(item["sha256"])
        _ = integer(item["bytes"], 1, maximum)
    helpers = exact(
        value["helperPins"],
        set(ops_guest.BASE | ops_guest.FETCH | ops_guest.FORCE) | {"security_vm_ops_guest.py"},
    )
    for sha in helpers.values():
        _ = digest(sha)
    return value


@final
class Control:
    """A nested duration clipped from the controller's one original fixture clock."""

    def __init__(self, seconds: int) -> None:
        """Reserve30 seconds without refreshing any controller or native budget."""
        self.started = time.monotonic()
        self.body_end = self.started + seconds
        self.end = self.body_end + 30
        self.status = 0
        self.settling = False

    def signal(self, number: int, _frame: object) -> None:
        """Latch cancellation without interrupting admitted owners."""
        self.status = self.status or (130 if number == signal.SIGINT else 143)

    def check(self) -> None:
        """Fence effects and cleanup against their original deadlines."""
        now = time.monotonic()
        require(
            math.isfinite(now)
            and self.started <= now < (self.end if self.settling else self.body_end)
        )
        require(not self.status or self.settling)


def native_budget(control: Control) -> int:
    """Keep explicit M60 and useful work inside the SAME remaining body."""
    control.check()
    seconds = min(600, math.floor(control.body_end - time.monotonic() - 20))
    require(seconds >= MINIMUM_NATIVE)
    return seconds


def adopt(descriptor: int, mode: str) -> BinaryIO:
    """Transfer an already-owned FD to a stream, closing it if adoption fails."""
    try:
        return cast("BinaryIO", os.fdopen(descriptor, mode))
    except BaseException as error:
        first = ops_guest.settle((lambda: os.close(descriptor),), error)
        if first is not None:
            raise first from None
        raise


@final
class Original:
    """Retain original regular files through all native and independent observations."""

    def __init__(self, path: Path, maximum: int, mode: int, control: Control) -> None:
        """Open nofollow and register the original stream before checking it."""
        self.path, self.control, self.closed = path, control, False
        control.check()
        self.file = adopt(
            os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK), "rb"
        )
        try:
            self.before = os.fstat(self.file.fileno())
            require(
                stat.S_ISREG(self.before.st_mode)
                and self.before.st_uid == self.before.st_gid == 0
                and stat.S_IMODE(self.before.st_mode) == mode
                and self.before.st_nlink == 1
                and 0 <= self.before.st_size <= maximum
            )
            self.sha256 = self.hash()
            self.check()
        except BaseException as error:
            first = ops_guest.settle((self.close,), error)
            if first is not None:
                raise first from None
            raise

    def hash(self) -> str:
        """Hash bounded chunks of the original descriptor, not a reopened name."""
        self.control.check()
        _ = self.file.seek(0)
        total, result = 0, hashlib.sha256()
        while True:
            self.control.check()
            chunk = self.file.read(min(65536, self.before.st_size + 1 - total))
            self.control.check()
            if not chunk:
                break
            total += len(chunk)
            require(total <= self.before.st_size)
            result.update(chunk)
        require(total == self.before.st_size)
        _ = self.file.seek(0)
        return result.hexdigest()

    def check(self) -> None:
        """Reject same-byte replacement and in-place changes around every read."""
        self.control.check()
        require(
            ops_guest.stamp(os.fstat(self.file.fileno())) == ops_guest.stamp(self.before)
            and ops_guest.stamp(self.path.lstat()) == ops_guest.stamp(self.before)
            and self.hash() == self.sha256
            and ops_guest.stamp(os.fstat(self.file.fileno())) == ops_guest.stamp(self.before)
        )

    def data(self, maximum: int = MAX_RECORD) -> bytes:
        """Read a bounded original record only after its identity fence."""
        self.check()
        require(self.before.st_size <= maximum)
        data = self.file.read(maximum + 1)
        self.check()
        require(len(data) == self.before.st_size)
        return data

    def pin(self) -> dict[str, object]:
        """Return only a nonsecret digest/length and original device/inode."""
        return {
            "sha256": self.sha256,
            "bytes": self.before.st_size,
            "device": self.before.st_dev,
            "inode": self.before.st_ino,
        }

    def close(self) -> None:
        """Attempt the original close once, including failure."""
        if not self.closed:
            self.closed = True
            self.file.close()


def archive_graph(path: Path, manifest: artifact.Manifest) -> tuple[set[str], dict[str, object]]:
    """Select only the authenticated config and exact selected OCI image manifest."""
    config = cast("dict[str, object]", artifact.verify_archive(path, manifest))
    with tarfile.open(path, "r:") as bundle:
        reader = artifact.ArchiveReader.indexed(bundle)
        images = reader.read_json("manifest.json")
        require(isinstance(images, list) and len(images) == 1)
        image = cast("dict[str, object]", cast("list[object]", images)[0])
        raw = reader.read_bytes(cast("str", image["Config"]))
        identities = {"sha256:" + hashlib.sha256(raw).hexdigest()}
        if "index.json" in reader.members:
            node = reader.read_json("index.json")
            for _ in range(artifact.MAX_INDEX_DEPTH):
                require(isinstance(node, dict))
                descriptors = cast("dict[str, object]", node).get("manifests")
                require(
                    isinstance(descriptors, list) and len(cast("list[object]", descriptors)) == 1
                )
                descriptor = cast("dict[str, object]", cast("list[object]", descriptors)[0])
                node = reader.read_descriptor(cast("JsonObject", descriptor))
                if descriptor.get("mediaType") in (
                    "application/vnd.oci.image.index.v1+json",
                    "application/vnd.docker.distribution.manifest.list.v2+json",
                ):
                    continue
                require(
                    descriptor.get("mediaType")
                    in (
                        "application/vnd.oci.image.manifest.v1+json",
                        "application/vnd.docker.distribution.manifest.v2+json",
                    )
                )
                identities.add(
                    "sha256:" + digest(str(descriptor["digest"]).removeprefix("sha256:"))
                )
                break
            else:
                require(None)
    return identities, config


def image_profile(
    value: object, identities: set[str], config: Mapping[str, object], revision: str
) -> str:
    """Inspect actual daemon identity/profile and ordered filesystem layer binding."""
    require(isinstance(value, list) and len(cast("list[object]", value)) == 1)
    image = cast("dict[str, object]", cast("list[object]", value)[0])
    runtime = cast("dict[str, object]", image.get("Config"))
    rootfs = cast("dict[str, object]", image.get("RootFS"))
    saved = cast("dict[str, object]", config.get("rootfs"))
    require(
        image.get("Id") in identities
        and image.get("Os") == "linux"
        and image.get("Architecture") == "amd64"
        and runtime.get("User") == "10001:10001"
        and runtime.get("Cmd") == ["/app/simplestChat"]
        and runtime.get("Entrypoint") in (None, [])
        and runtime.get("WorkingDir") == "/app"
        and not runtime.get("Healthcheck")
        and cast("dict[str, object]", runtime.get("Labels")).get(
            "org.opencontainers.image.revision"
        )
        == revision
        and rootfs.get("Type") == "layers"
        and rootfs.get("Layers") == saved.get("diff_ids")
    )
    variables = runtime.get("Env")
    require(
        isinstance(variables, list)
        and all(isinstance(item, str) for item in cast("list[object]", variables))
    )
    runtime_profile.validate_environment_names(
        [str(item).split("=", 1)[0] for item in cast("list[object]", variables)]
    )
    return cast("str", image["Id"])


def migration_map(data: bytes) -> dict[str, str]:
    """Accept only unique nonempty fixed-path SHA384 migration rows."""
    result: dict[str, str] = {}
    require(len(data) <= MAX_STREAM)
    for line in data.decode("ascii").splitlines():
        match = re.fullmatch(r"([a-f0-9]{96})  /app/migrations/([0-9]+)_[A-Za-z0-9_]+\.sql", line)
        require(match is not None)
        if match is not None:
            version = str(int(match[2]))
            require(version not in result and len(result) < MAX_MIGRATIONS)
            result[version] = match[1]
    require(result)
    return result


def actor_sequence(
    data: bytes, name: str, image: str, expected: str | None = None
) -> tuple[str, list[str], int]:
    """Require a complete native actor lifecycle and retain it as an eviction anchor."""
    require(len(data) <= MAX_STREAM)
    rows = data.splitlines()
    require(0 < len(rows) < EVENT_HISTORY)
    actors: dict[str, list[str]] = {}
    for line in rows:
        value = record(line)
        if value.get("Type") != "container":
            continue
        actor = cast("dict[str, object]", value.get("Actor"))
        attributes = cast("dict[str, object]", actor.get("Attributes"))
        if attributes.get("name") != name:
            # No unrelated container work is admitted in these native windows.
            require(None)
            continue
        identifier = digest(actor.get("ID"))
        require(attributes.get("image") == image)
        action = value.get("Action")
        if action in ("create", "start", "die", "destroy"):
            if action == "die":
                require(attributes.get("exitCode") == "0")
            actors.setdefault(identifier, []).append(cast("str", action))
    require(len(actors) == 1)
    identifier, actions = next(iter(actors.items()))
    require(
        actions == ["create", "start", "die", "destroy"]
        and (expected is None or identifier == expected)
    )
    return identifier, actions, len(rows)


@final
class Runner:
    """Keep fixed FD/Docker children and private captures inside the final VM owner.

    Normal pump completion proves direct leader status and pipe EOF; it does not
    prove whole-group retirement after consuming reap. The exact final VM owner
    retires remaining guest descendants. Never signal a consumed PID/group.
    """

    def __init__(self, control: Control, originals: list[Original]) -> None:
        """Keep command owners with the outer whole-guest transaction."""
        self.control, self.originals = control, originals
        self.count = 0
        self.last_status: int | None = None

    def healthy(self) -> bool:
        """Poll original cancellation/deadline while child pipes are idle."""
        try:
            self.control.check()
        except ValueError:
            return False
        return True

    def command(
        self,
        arguments: Sequence[str],
        *,
        candidate: Original | None = None,
        timeout: float = 30,
        accepted: tuple[int, ...] = (0,),
    ) -> tuple[int, bytes]:
        """Close every child stream and original private log after any first failure."""
        self.control.check()
        for original in self.originals:
            require(
                ops_guest.stamp(os.fstat(original.file.fileno()))
                == ops_guest.stamp(original.before)
                and ops_guest.stamp(original.path.lstat()) == ops_guest.stamp(original.before)
            )
        self.count += 1
        require(self.count <= MAX_COMMANDS)
        streams: list[BinaryIO] = []
        child: subprocess.Popen[bytes] | None = None
        first: BaseException | None = None
        consumed = False
        status, output = 1, b""
        try:
            for suffix in ("stdout", "stderr"):
                path = ROOT / f"stage-observer-{self.count:03}.{suffix}"
                descriptor = os.open(
                    path, os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600
                )
                streams.append(adopt(descriptor, "w+b"))
            if candidate is not None:
                candidate.check()
            child = subprocess.Popen(  # noqa: S603 -- Fixed argv/authenticated FD.
                ["simplestchat-ops", *arguments]
                if candidate is not None
                else [*DOCKER, *arguments],
                executable="/proc/self/fd/" + str(candidate.file.fileno())
                if candidate is not None
                else DOCKER[0],
                pass_fds=(candidate.file.fileno(),) if candidate is not None else (),
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
                cwd=ROOT,
                env=ENV,
            )
            ceiling = self.control.end if self.control.settling else self.control.body_end
            remaining = min(timeout, ceiling - time.monotonic())
            require(remaining > 0)
            status = bounded_process.pump(
                child,
                streams[0],
                streams[1],
                bounded_process.Limits(
                    timeout=remaining, stdout=MAX_STREAM, stderr=MAX_STREAM, healthy=self.healthy
                ),
            )
            consumed = True
            self.last_status = status
            for stream in streams:
                stream.flush()
            _ = streams[0].seek(0)
            output = streams[0].read(MAX_STREAM + 1)
            require(len(output) <= MAX_STREAM and status in accepted)
            if candidate is not None:
                candidate.check()
        except BaseException as error:  # noqa: BLE001 -- Keep first; settle all peers.
            first = error
            if child is not None and not consumed and child.returncode is None:
                first = ops_guest.settle((lambda: bounded_process.stop(child),), first)
        finally:
            finalizers: list[Callable[[], None]] = []
            if child is not None:
                finalizers.extend(
                    stream.close
                    for stream in (child.stdin, child.stdout, child.stderr)
                    if stream is not None
                )
            finalizers.extend(stream.close for stream in streams)
            first = ops_guest.settle(finalizers, first)
        if first is not None:
            raise first
        self.control.check()
        return status, output


def utc() -> str:
    """Wall time selects history only; all deadline authority stays monotonic."""
    return (
        datetime.datetime.now(datetime.UTC)
        .isoformat(timespec="microseconds")
        .replace("+00:00", "Z")
    )


def stage_report(data: bytes, revision: str) -> Path:
    """Candidate JSON remains passive until real0 and every outer finalizer."""
    value = exact(
        record(data),
        {"evidence", "action", "revision", "startedAt", "passed", "phase", "finishedAt"},
    )
    require(
        value["action"] == "stage"
        and value["revision"] == revision
        and value["passed"] is True
        and value["phase"] == "complete"
    )
    for key in ("startedAt", "finishedAt"):
        require(
            isinstance(value[key], str)
            and re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ", cast("str", value[key]))
        )
    require(isinstance(value["evidence"], str))
    path = Path(cast("str", value["evidence"]))
    require(path.parent == PUBLIC / "results" and re.fullmatch(r"release\.[a-f0-9]{32}", path.name))
    metadata = path.lstat()
    require(
        stat.S_ISDIR(metadata.st_mode)
        and metadata.st_uid == metadata.st_gid == 0
        and stat.S_IMODE(metadata.st_mode) == DIRECTORY_MODE
    )
    return path


def probe_profile(value: object, identifier: str, image: str, name: str, token: str) -> None:
    """Only this independently retained probe's actual isolation is claimed."""
    require(isinstance(value, list) and len(cast("list[object]", value)) == 1)
    row = cast("dict[str, object]", cast("list[object]", value)[0])
    config = cast("dict[str, object]", row.get("Config"))
    host = cast("dict[str, object]", row.get("HostConfig"))
    labels = cast("dict[str, object]", config.get("Labels"))
    logging = cast("dict[str, object]", host.get("LogConfig"))
    require(
        row.get("Id") == identifier
        and row.get("Name") == "/" + name
        and row.get("Image") == image
        and labels.get("simplestchat.stage-proof") == token
        and config.get("User") == "10001:10001"
        and config.get("Entrypoint") == ["/usr/bin/timeout"]
        and config.get("Cmd")
        == [
            "--signal=TERM",
            "--kill-after=2s",
            "10s",
            "/bin/sh",
            "-c",
            'for file in /app/migrations/*.sql; do sha384sum "$file" || exit; done',
        ]
        and logging.get("Type") == "local"
        and logging.get("Config") == {"max-size": "1m", "max-file": "1", "compress": "false"}
        and host.get("Init") is True
        and host.get("NetworkMode") == "none"
        and host.get("ReadonlyRootfs") is True
        and host.get("Privileged") is False
        and host.get("CapDrop") == ["ALL"]
        and host.get("CapAdd") in (None, [])
        and host.get("SecurityOpt") in (["no-new-privileges"], ["no-new-privileges:true"])
        and host.get("Memory") == 128 * 1024**2
        and host.get("NanoCpus") == PROBE_NANO_CPUS
        and host.get("PidsLimit") == PROBE_PIDS
        and not row.get("Mounts")
    )


def separate_probe(
    runner: Runner, image: str, migrations: Mapping[str, str], token: str
) -> dict[str, object]:
    """Inspect/start/wait/hash/retire one exact independently owned checksum container."""
    name = "scpub-stage-proof-" + token
    identifier: str | None = None
    first: BaseException | None = None
    observed: dict[str, object] = {}
    try:
        _, data = runner.command(
            [
                "create",
                "--name",
                name,
                "--label",
                "simplestchat.stage-proof=" + token,
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
                image,
                "--signal=TERM",
                "--kill-after=2s",
                "10s",
                "/bin/sh",
                "-c",
                'for file in /app/migrations/*.sql; do sha384sum "$file" || exit; done',
            ]
        )
        identifier = digest(data.decode("ascii").strip())
        _, inspection = runner.command(["inspect", identifier])
        probe_profile(cast("object", json.loads(inspection)), identifier, image, name, token)
        _ = runner.command(["start", identifier])
        _, waited = runner.command(["wait", identifier], timeout=20)
        require(waited.strip() == b"0")
        _, logs = runner.command(["logs", identifier])
        require(migration_map(logs) == migrations)
        _, inspection = runner.command(["inspect", identifier])
        values = cast("list[dict[str, object]]", json.loads(inspection))
        probe_profile(values, identifier, image, name, token)
        state = cast("dict[str, object]", values[0]["State"])
        require(
            state.get("Running") is False
            and state.get("OOMKilled") is False
            and state.get("ExitCode") == 0
            and not state.get("Error")
        )
        observed = {
            "passed": True,
            "isolationIndependentlyInspected": True,
            "migrationMapMatched": True,
            "exitStatus": 0,
            "containerId": identifier,
        }
    except BaseException as error:  # noqa: BLE001 -- Always attempt known cleanup.
        first = error
    finally:
        if identifier is not None:
            previous = runner.control.settling
            runner.control.settling = True

            def remove() -> None:
                _, inspection = runner.command(["inspect", identifier])
                probe_profile(
                    cast("object", json.loads(inspection)), identifier, image, name, token
                )
                _ = runner.command(["rm", "--force", identifier])
                _, listed = runner.command(
                    ["ps", "--all", "--quiet", "--no-trunc", "--filter", "id=" + identifier]
                )
                require(not listed.strip())

            first = ops_guest.settle((remove,), first)
            runner.control.settling = previous
    if first is not None:
        raise first
    return observed


def retain(
    path: Path, maximum: int, mode: int, control: Control, originals: list[Original]
) -> Original:
    """Register each admitted original immediately with the whole operation."""
    value = Original(path, maximum, mode, control)
    originals.append(value)
    return value


def outcome(attempt: Path, revision: str, control: Control, originals: list[Original]) -> None:
    """Bind the real attempt record and finite exact command stdout/stderr census."""
    value = record(retain(attempt / "outcome.json", MAX_RECORD, 0o600, control, originals).data())
    require(
        set(value) == {"action", "revision", "startedAt", "passed", "phase", "finishedAt"}
        and value.get("action") == "stage"
        and value.get("revision") == revision
        and value.get("passed") is True
        and value.get("phase") == "complete"
    )


def command_records(
    attempt: Path, count: int, control: Control, originals: list[Original]
) -> list[bytes]:
    """Source-bound fixed command ordinals support effects, without inventing argv logs."""
    expected = {"outcome.json"} | {
        f"{i:03}.{s}" for i in range(1, count + 1) for s in ("stdout", "stderr")
    }
    if count == FIRST_COMMANDS:
        expected.add("validation-name.txt")
    require({path.name for path in attempt.iterdir()} == expected)
    result: list[bytes] = []
    for i in range(1, count + 1):
        result.append(
            retain(attempt / f"{i:03}.stdout", MAX_STREAM, 0o600, control, originals).data(
                MAX_STREAM
            )
        )
        _ = retain(attempt / f"{i:03}.stderr", MAX_STREAM, 0o600, control, originals).data(
            MAX_STREAM
        )
    return result


def snapshot_sets(runner: Runner) -> dict[str, list[str]]:
    """Observe daemon-wide IDs without removing any unknown resource."""
    values: dict[str, list[str]] = {}
    for role, arguments in (
        ("containers", ["ps", "--all", "--quiet", "--no-trunc"]),
        ("networks", ["network", "ls", "--quiet", "--no-trunc"]),
        ("volumes", ["volume", "ls", "--quiet"]),
    ):
        _, raw = runner.command(arguments)
        values[role] = sorted(raw.decode("ascii").splitlines())
    return values


def execute(  # noqa: PLR0915 -- Ordered owners and finalizers.
    value: dict[str, object], control: Control
) -> dict[str, object]:
    """Keep all retained files, daemon effects and peer finalizers in one transaction."""
    originals: list[Original] = []
    first: BaseException | None = None
    result: dict[str, object] = {}
    phase = "guard"
    runner: Runner | None = None
    try:
        require(os.geteuid() == 0 and sys.platform == "linux")
        marker, _ = ops_guest.read_fixed(MARKER, 128, 0o600)
        require(marker == (str(value["runId"]) + "\n").encode())
        distribution = Path("/etc/os-release").read_text()
        require(
            re.search(r"^ID=\"?debian\"?$", distribution, re.MULTILINE)
            and re.search(r"^VERSION_ID=\"?13\"?$", distribution, re.MULTILINE)
        )
        self_source = retain(ROOT / "security_vm_stage_guest.py", 2**20, 0o600, control, originals)
        require(self_source.sha256 == value["guestSha256"])
        revision = cast("str", value["revision"])
        directory = PUBLIC / "releases" / revision
        for path in (
            PUBLIC,
            PUBLIC / "releases",
            PUBLIC / "results",
            directory,
            Path("/etc/simplestchat-public"),
        ):
            metadata = path.lstat()
            require(
                stat.S_ISDIR(metadata.st_mode)
                and metadata.st_uid == metadata.st_gid == 0
                and stat.S_IMODE(metadata.st_mode) == DIRECTORY_MODE
            )
        installed = retain(INSTALLED, ops_guest.MAX_BINARY, 0o755, control, originals)
        manifest_original = retain(directory / "release.json", 2**20, 0o600, control, originals)
        archive_original = retain(directory / "image.tar", MAX_IMAGE, 0o600, control, originals)
        for label, original in (
            ("binary", installed),
            ("manifest", manifest_original),
            ("archive", archive_original),
        ):
            expected = cast("dict[str, object]", value[label])
            require(
                original.sha256 == expected["sha256"]
                and original.before.st_size == expected["bytes"]
            )
        _ = retain(Path("/usr/bin/docker"), 320 * 1024**2, 0o755, control, originals)
        socket_path = Path("/var/run/docker.sock").resolve(strict=True)
        socket_stamp = socket_path.lstat()
        require(stat.S_ISSOCK(socket_stamp.st_mode) and socket_stamp.st_uid == 0)
        helpers = cast("dict[str, object]", value["helperPins"])
        for name, sha in helpers.items():
            path = ROOT / name if name == "security_vm_ops_guest.py" else LIBRARY / name
            original = retain(
                path,
                2**20,
                0o600 if name == "security_vm_ops_guest.py" else 0o644,
                control,
                originals,
            )
            require(original.sha256 == sha)
        protected = [
            retain(Path("/etc/simplestchat-public") / name, 65536, 0o600, control, originals)
            for name in ("images.json", "app.env", "migration.env", "compose.public.yml")
        ]
        manifest = artifact.validate_manifest(manifest_original.path)
        require(manifest["revision"] == revision)
        identities, config = archive_graph(archive_original.path, manifest)
        for original in originals:
            original.check()
        runner = Runner(control, originals)
        baseline = snapshot_sets(runner)
        require(not baseline["containers"] and not baseline["volumes"])
        _, tags = runner.command(
            [
                "image",
                "ls",
                "--quiet",
                "--no-trunc",
                "--filter",
                "reference=" + manifest["imageTag"],
            ]
        )
        require(not tags.strip() and not (directory / "staged.json").exists())
        before = {path.name for path in (PUBLIC / "results").iterdir()}
        since = utc()
        n = native_budget(control)
        phase = "native-first"
        _, data = runner.command(
            [
                "release",
                "stage",
                "--revision",
                revision,
                "--timeout-seconds",
                str(n),
                "--finalization-seconds",
                "60",
            ],
            candidate=installed,
            timeout=n + 10,
        )
        phase = "observe-first"
        first_until = utc()
        require(first_until >= since)
        attempt = stage_report(data, revision)
        require({path.name for path in (PUBLIC / "results").iterdir()} - before == {attempt.name})
        outcome(attempt, revision, control, originals)
        outputs = command_records(attempt, 7, control, originals)
        staged = retain(directory / "staged.json", 16384, 0o600, control, originals)
        staged_value = exact(
            record(staged.data()),
            {"schemaVersion", "revision", "serverImage", "manifestSha256", "stagedAt"},
        )
        image = cast("str", staged_value["serverImage"])
        require(
            type(staged_value["schemaVersion"]) is int
            and staged_value["schemaVersion"] == 1
            and staged_value["revision"] == revision
            and image in identities
            and staged_value["manifestSha256"] == manifest_original.sha256
            and isinstance(staged_value["stagedAt"], str)
        )
        journal = retain(PUBLIC / "release-state.json", 16384, 0o600, control, originals)
        journal_value = exact(
            record(journal.data()), {"schemaVersion", "attempt", "finalized", "phase"}
        )
        require(
            journal_value
            == {"schemaVersion": 1, "attempt": str(attempt), "finalized": True, "phase": "staged"}
        )
        name = (
            retain(attempt / "validation-name.txt", 128, 0o600, control, originals)
            .data()
            .decode("ascii")
            .strip()
        )
        require(re.fullmatch("scpub-release-validate-[a-f0-9]{32}", name))
        actor = digest(outputs[2].decode("ascii").strip())
        require(outputs[4].strip() == b"0" and migration_map(outputs[5]) == manifest["migrations"])
        _, events = runner.command(
            ["events", "--since", since, "--until", first_until, "--format", "{{json .}}"]
        )
        actor_id, sequence, first_events = actor_sequence(events, name, image, actor)
        _, inspected = runner.command(["image", "inspect", image])
        require(
            image_profile(cast("object", json.loads(inspected)), identities, config, revision)
            == image
        )
        require(snapshot_sets(runner) == baseline)
        before_repeat = {path.name for path in (PUBLIC / "results").iterdir()}
        staged_before = staged.pin()
        n = native_budget(control)
        phase = "native-repeat"
        _, data = runner.command(
            [
                "release",
                "stage",
                "--revision",
                revision,
                "--timeout-seconds",
                str(n),
                "--finalization-seconds",
                "60",
            ],
            candidate=installed,
            timeout=n + 10,
        )
        phase = "observe-repeat"
        repeat_until = utc()
        require(repeat_until >= first_until)
        repeat = stage_report(data, revision)
        require(
            repeat != attempt
            and {path.name for path in (PUBLIC / "results").iterdir()} - before_repeat
            == {repeat.name}
        )
        outcome(repeat, revision, control, originals)
        _ = command_records(repeat, 1, control, originals)
        staged.check()
        journal.check()
        require(staged.pin() == staged_before)
        _, events = runner.command(
            ["events", "--since", since, "--until", repeat_until, "--format", "{{json .}}"]
        )
        repeated_actor, repeated_sequence, total_events = actor_sequence(
            events, name, image, actor_id
        )
        require(repeated_actor == actor_id and repeated_sequence == sequence)
        phase = "separate-probe"
        probe = separate_probe(runner, image, manifest["migrations"], cast("str", value["runId"]))
        require(snapshot_sets(runner) == baseline)
        _, inspected = runner.command(["image", "inspect", image])
        require(
            image_profile(cast("object", json.loads(inspected)), identities, config, revision)
            == image
        )
        phase = "final-observation"
        lock = os.open(WORK / "workload.lock", os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC)
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            fcntl.flock(lock, fcntl.LOCK_UN)
        finally:
            os.close(lock)
        for original in originals:
            original.check()
        require(
            socket_path == Path("/var/run/docker.sock").resolve(strict=True)
            and (
                socket_stamp.st_dev,
                socket_stamp.st_ino,
                socket_stamp.st_mode,
                socket_stamp.st_uid,
                socket_stamp.st_gid,
            )
            == (
                socket_path.lstat().st_dev,
                socket_path.lstat().st_ino,
                socket_path.lstat().st_mode,
                socket_path.lstat().st_uid,
                socket_path.lstat().st_gid,
            )
        )
        result = {
            "schemaVersion": 1,
            "scope": "SignedMuslStageGuest",
            "runId": value["runId"],
            "revision": revision,
            "passed": True,
            "firstExitStatus": 0,
            "repeatExitStatus": 0,
            "staged": staged.pin(),
            "manifestSha256": manifest_original.sha256,
            "imageId": image,
            "nativeActorId": actor_id,
            "nativeActorLifecycleObserved": True,
            "nativeValidatorIsolationIndependentlyInspected": False,
            "firstEventCount": first_events,
            "cumulativeEventCount": total_events,
            "repeatStagedOriginalUnchanged": True,
            "repeatSkippedLoadAndValidator": True,
            "migrationMapMatched": True,
            "knownContainersAbsent": True,
            "protectedFilesUnchanged": len(protected),
            "workloadLockReacquired": True,
            "separateProbe": probe,
            "commands": runner.count,
        }
    except BaseException as error:  # noqa: BLE001 -- Settle every known owner.
        first = error
    finally:
        control.settling = True
        actions = [action for original in originals for action in (original.check, original.close)]
        first = ops_guest.settle((*actions, control.check), first)
        diagnostic = {
            "schemaVersion": 1,
            "phase": phase,
            "passed": first is None,
            "firstFailureKind": None if first is None else type(first).__name__,
            "lastCommand": 0 if runner is None else runner.count,
            "lastSignedStatus": None if runner is None else runner.last_status,
        }

        def retain_diagnostic() -> None:
            _ = ops_guest.write_new(
                ROOT / "stage-observer-diagnostic.json", (json.dumps(diagnostic) + "\n").encode()
            )

        first = ops_guest.settle((retain_diagnostic,), first)
    if first is not None:
        raise first
    return result


def private_evidence(run_id: str) -> bytes:
    """Retain fixed bounded private prefixes before VM retirement; never upload this."""
    require(os.geteuid() == 0 and re.fullmatch("[a-f0-9]{32}", run_id))
    marker, _ = ops_guest.read_fixed(MARKER, 128, 0o600)
    require(marker == (run_id + "\n").encode())
    paths: list[tuple[str, int, str, Path]] = []
    for ordinal in range(1, 41):
        for stream in ("stdout", "stderr"):
            path = ROOT / f"stage-observer-{ordinal:03}.{stream}"
            if path.exists() or path.is_symlink():
                paths.append(("observer", ordinal, stream, path))
    attempts = sorted((PUBLIC / "results").glob("release.*"))
    require(len(attempts) <= MAX_EVIDENCE_ATTEMPTS)
    for ordinal, attempt in enumerate(attempts, 1):
        require(re.fullmatch(r"release\.[a-f0-9]{32}", attempt.name))
        for path in sorted(attempt.iterdir()):
            require(
                path.name in {"outcome.json", "validation-name.txt"}
                or re.fullmatch(r"0(?:0[1-9]|1[0-9]|20)\.(stdout|stderr)", path.name)
            )
            paths.append(("native-attempt", ordinal, path.name, path))
    diagnostic = ROOT / "stage-observer-diagnostic.json"
    if diagnostic.exists():
        paths.append(("diagnostic", 0, "fixed", diagnostic))
    require(len(paths) <= MAX_EVIDENCE_FILES)
    rows: list[dict[str, object]] = []
    for role, ordinal, stream, path in paths:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
        original = adopt(descriptor, "rb")
        first: BaseException | None = None
        before: os.stat_result | None = None
        prefix = b""
        try:
            before = os.fstat(original.fileno())
            require(
                stat.S_ISREG(before.st_mode)
                and before.st_uid == before.st_gid == 0
                and stat.S_IMODE(before.st_mode) == PRIVATE_MODE
                and before.st_nlink == 1
                and 0 <= before.st_size <= MAX_STREAM
            )
            prefix = original.read(8192)
            require(
                ops_guest.stamp(before) == ops_guest.stamp(os.fstat(original.fileno()))
                and ops_guest.stamp(before) == ops_guest.stamp(path.lstat())
            )
        except BaseException as error:  # noqa: BLE001 -- Keep body error through close.
            first = error
        finally:
            first = ops_guest.settle((original.close,), first)
        if first is not None:
            raise first
        require(before is not None)
        before = cast("os.stat_result", before)
        rows.append(
            {
                "role": role,
                "ordinal": ordinal,
                "stream": stream,
                "bytes": before.st_size,
                "prefixBase64": base64.b64encode(prefix).decode("ascii"),
                "truncated": before.st_size > len(prefix),
            }
        )
    data = (
        json.dumps({"schemaVersion": 1, "scope": "StageVmPrivateEvidence", "files": rows}) + "\n"
    ).encode()
    require(len(data) <= MAX_STREAM)
    return data


def main() -> int:  # noqa: C901 -- Fixed diagnostic branch plus explicit all-owner restoration.
    """Preserve original status through all restores and final stdout fences."""
    if len(sys.argv) == EVIDENCE_ARGC and sys.argv[1] == "--private-evidence":
        try:
            evidence = private_evidence(sys.argv[2])
            require(sys.stdout.buffer.write(evidence) == len(evidence))
            _ = sys.stdout.buffer.flush()
        except (OSError, ValueError, RuntimeError):
            return 1
        return 0
    require(len(sys.argv) == 1)
    started = time.monotonic()
    control: Control | None = None
    handlers: dict[signal.Signals, object] = {}
    mask = os.umask(0o077)
    status = 1
    try:
        value = request(sys.stdin.buffer.read(MAX_RECORD + 1))
        control = Control(cast("int", value["bodySeconds"]))
        control.started = started
        control.body_end = started + cast("int", value["bodySeconds"])
        control.end = control.body_end + 30
        for number in (signal.SIGINT, signal.SIGTERM):
            handlers[number] = signal.signal(number, control.signal)
        observed = execute(value, control)
        data = (json.dumps(observed, separators=(",", ":"), allow_nan=False) + "\n").encode()
        require(len(data) <= MAX_RECORD and sys.stdout.buffer.write(data) == len(data))
        _ = sys.stdout.buffer.flush()
        status = 0
    except (OSError, ValueError, RuntimeError, KeyError, TypeError, AttributeError):
        _ = sys.stderr.write("Signed musl Stage guest proof is unproven.\n")
    finally:
        actions: list[Callable[[], None]] = []
        for number, handler in handlers.items():

            def restore(number: signal.Signals = number, handler: object = handler) -> None:
                _ = signal.signal(
                    number, cast("int | Callable[[int, object], None] | None", handler)
                )

            actions.append(restore)

        def restore_mask() -> None:
            _ = os.umask(mask)

        actions.append(restore_mask)
        if ops_guest.settle(actions) is not None:
            status = 1
    if control is not None:
        try:
            control.check()
        except ValueError:
            status = 1
        status = control.status or status
    return status


if __name__ == "__main__":
    raise SystemExit(main())
