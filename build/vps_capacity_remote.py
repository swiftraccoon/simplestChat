"""Private capacity host protocol and bounded, journaled systemd worker.

The controller transfers this committed module over SSH stdin. All request
values are validated data; only fixed commands and explicit argv are executed.
The worker never removes containers: capacity.py owns normal cleanup, and
uncertain residuals are retained with exact identities for inspection.
"""

# Failure codes cross an SSH boundary and intentionally contain no host secrets.
# ruff: noqa: EM101

from __future__ import annotations

import fcntl
import json
import math
import os
import re
import select
import signal
import stat
import subprocess
import sys
import tarfile
import tempfile
from contextlib import contextmanager
from pathlib import Path
from typing import TYPE_CHECKING, cast

if TYPE_CHECKING:
    from collections.abc import Generator
    from types import FrameType

ROOT = Path("/srv/simplestchat-bench")
STATE = Path("/run/simplestchat-bench")
DOCKER = ["/usr/bin/docker", "--host", "unix:///var/run/docker.sock"]
BUILD_UNIT = "simplestchat-image-build.service"
MAX_ARCHIVE = 512 * 1024**2
MAX_EXPANDED = 1024**3
MAX_FILE = 128 * 1024**2
MAX_FILES = 10000
MAX_JSON = 8 * 1024**2
MIN_CHAT_INTERVAL = 1000
MAX_RECOVERY_CONTAINERS = 6
ACTIVE = frozenset({"active", "activating", "deactivating", "reloading"})
ENVIRONMENT = {"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LC_ALL": "C", "HOME": "/root"}


class CapacityControlError(Exception):
    """A failed invariant; evidence is retained and no workload is retried."""


def require(condition: object, code: str) -> None:
    """Reject ambiguous input or host state with a stable error code."""
    if not condition:
        raise CapacityControlError(code)


def obj(value: object) -> dict[str, object]:
    """Require a JSON object without coercing an untrusted value."""
    require(isinstance(value, dict), "expected_object")
    return cast("dict[str, object]", value)


def array(value: object) -> list[object]:
    """Require a JSON array without coercing an untrusted value."""
    require(isinstance(value, list), "expected_array")
    return cast("list[object]", value)


def text(value: object) -> str:
    """Require a string before using it at a path or command boundary."""
    require(isinstance(value, str), "expected_string")
    return cast("str", value)


def number(value: object, minimum: float, maximum: float) -> float:
    """Require a finite number in the documented operational range."""
    require(type(value) in (int, float), "invalid_number")
    result = float(cast("int | float", value))
    require(math.isfinite(result) and minimum <= result <= maximum, "invalid_number")
    return result


def integer(value: object, minimum: int, maximum: int) -> int:
    """Reject booleans and fractional operational limits."""
    require(type(value) is int, "invalid_integer")
    _ = number(value, minimum, maximum)
    return cast("int", value)


def protected(path: Path, *, directory: bool = False, private: bool = True) -> None:
    """Require existing root-owned paths without symlink components."""
    info = path.lstat()
    require(path.resolve(strict=True) == path, "symlink_path")
    require(info.st_uid == 0, "unowned_path")
    require(
        stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode), "wrong_file_kind"
    )
    require(not info.st_mode & 0o022, "writable_path")
    if private:
        require(stat.S_IMODE(info.st_mode) == (0o700 if directory else 0o600), "nonprivate_path")


def read_json(path: Path) -> dict[str, object]:
    """Read a bounded regular JSON report without following a symlink."""
    require(not path.is_symlink() and path.is_file(), "invalid_json_file")
    require(path.stat().st_size <= MAX_JSON, "json_too_large")
    return obj(cast("object", json.loads(path.read_bytes())))


def save(path: Path, value: dict[str, object]) -> None:
    """Atomically replace a private journal or evidence record."""
    temporary = path.with_suffix(path.suffix + ".tmp")
    require(not temporary.is_symlink(), "symlink_temporary")
    _ = temporary.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")
    temporary.chmod(0o600)
    _ = temporary.replace(path)


def command(argv: list[str], *, timeout: int = 30, allow_failure: bool = False) -> str:
    """Run fixed argv with an isolated environment and bounded captured output."""
    with tempfile.TemporaryFile() as output:
        result = subprocess.run(  # noqa: S603 -- validated explicit argv; no shell.
            argv,
            stdin=subprocess.DEVNULL,
            stdout=output,
            stderr=output,
            env=ENVIRONMENT,
            timeout=timeout,
            check=False,
        )
        _ = output.seek(0)
        data = output.read(MAX_JSON + 1)
    require(len(data) <= MAX_JSON, "command_output_too_large")
    require(allow_failure or result.returncode == 0, "host_command_failed")
    return data.decode("utf-8", errors="strict").strip()


def unit_state(unit: str) -> dict[str, object]:
    """Read retained unit result fields even for failed or inactive units."""
    output = command(
        [
            "/usr/bin/systemctl",
            "show",
            unit,
            "--property=LoadState,ActiveState,SubState,Result,ExecMainCode,ExecMainStatus,MainPID",
        ],
        allow_failure=True,
    )
    state: dict[str, object] = {
        key: value
        for line in output.splitlines()
        if (key := line.partition("=")[0])
        for value in [line.partition("=")[2]]
    }
    require(
        state.get("LoadState") in ("loaded", "not-found")
        and "ActiveState" in state
        and "MainPID" in state,
        "invalid_unit_state",
    )
    return state


def unit_finished(state: dict[str, object]) -> bool:
    """Recognize inactive/failed units and retained success without a live process."""
    return state.get("ActiveState") in ("inactive", "failed") or (
        state.get("ActiveState") == "active"
        and state.get("SubState") == "exited"
        and state.get("MainPID") == "0"
    )


def container_ids(*, running: bool = False, label: str | None = None) -> list[str]:
    """Snapshot immutable container IDs, never abbreviated IDs or name prefixes."""
    args = [*DOCKER, "ps", "--quiet", "--no-trunc"]
    if not running:
        args.append("--all")
    if label is not None:
        args.extend(["--filter", f"label={label}"])
    ids = command(args).splitlines()
    require(all(re.fullmatch(r"[a-f0-9]{64}", item) for item in ids), "invalid_container_id")
    return sorted(ids)


def preflight(*, allowed_unit: str | None = None) -> dict[str, object]:
    """Refuse public hosts, active builds/loads and unfinished cleanup journals."""
    require(os.geteuid() == 0, "root_required")
    require(not Path("/etc/simplestchat-public/compose.public.yml").exists(), "public_host_refused")
    require(
        not container_ids(label="com.docker.compose.project=simplestchat-public"),
        "public_project_refused",
    )
    for name in ("simplestchat-benchmark.service", BUILD_UNIT):
        require(unit_state(name).get("ActiveState") not in ACTIVE, "host_unit_busy")
    active = command(
        [
            "/usr/bin/systemctl",
            "list-units",
            "--all",
            "--no-legend",
            "--plain",
            "--state=active,activating,deactivating,reloading",
            "simplestchat-capacity-*.service",
        ]
    )
    require(
        all(line.split()[0] == allowed_unit for line in active.splitlines()), "capacity_unit_busy"
    )
    require(not container_ids(running=True), "running_containers_refused")
    current = STATE / "current.json"
    if current.exists() or current.is_symlink():
        protected(current)
        journal = read_json(current)
        require(
            journal.get("schemaVersion") == 1 and journal.get("finalized") is True,
            "cleanup_unfinished",
        )
    for path in (ROOT / "sources", ROOT / "artifacts", ROOT / "results"):
        protected(path, directory=True)
    return {"containersBefore": container_ids(), "privateHost": True}


@contextmanager
def workload_lock() -> Generator[None]:
    """Hold the same inode used by canonical image, benchmark and release helpers."""
    STATE.mkdir(mode=0o700, exist_ok=True)
    protected(STATE, directory=True)
    path = STATE / "workload.lock"
    if not path.exists():
        descriptor = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        os.close(descriptor)
    protected(path)
    with path.open("rb") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield


def validate_request(value: dict[str, object]) -> dict[str, object]:
    """Validate every workload selector before constructing subprocess arguments."""
    require(re.fullmatch(r"[a-f0-9]{40}", text(value.get("revision"))), "invalid_revision")
    require(re.fullmatch(r"[a-f0-9]{32}", text(value.get("run"))), "invalid_run_id")
    require(value.get("workload") in ("meetings", "large-meeting", "webinar"), "invalid_workload")
    require(
        re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,63}", text(value.get("label"))), "invalid_label"
    )
    for name, limits in {
        "firstSize": (2, 10000),
        "steps": (1, 12),
        "meetingSize": (2, 64),
        "speakers": (1, 64),
        "runtimeSeconds": (120, 21600),
    }.items():
        _ = integer(value.get(name), *limits)
    require(
        value.get("quick") in (True, False) and type(value.get("quick")) is bool, "invalid_quick"
    )
    require(type(value.get("audioOnly")) is bool, "invalid_audio_only")
    interval = integer(value.get("chatIntervalMs"), 0, 600000)
    require(
        interval == 0
        or MIN_CHAT_INTERVAL <= interval <= ((30000 if value["quick"] else 60000) - 2000),
        "chat_window_too_short",
    )
    for name in ("serverCpus", "generatorCpus", "appCpus"):
        if value.get(name) is not None:
            _ = number(value[name], 0.1, 256)
    if value.get("monthlyPrice") is not None:
        _ = number(value["monthlyPrice"], 0.01, 1000000)
    _ = number(value.get("portMbps", 1000), 1, 1000000)
    return value


def evidence_path(request: dict[str, object]) -> Path:
    """Derive the only writable evidence path from a validated random run ID."""
    run = text(request.get("run"))
    require(re.fullmatch(r"[a-f0-9]{32}", run), "invalid_run_id")
    return ROOT / "results" / f"capacity-controller.{run}"


def capacity_unit_state(request: dict[str, object]) -> dict[str, object]:
    """Recover a collected unit's terminal result after systemd garbage collection."""
    unit = f"simplestchat-capacity-{request['run']}.service"
    state = unit_state(unit)
    if state.get("LoadState") != "not-found":
        return state
    directory = evidence_path(request)
    protected(directory, directory=True)
    saved = read_json(directory / "request.json")
    require(
        saved.get("revision") == request.get("revision") and saved.get("run") == request.get("run"),
        "collection_revision_mismatch",
    )
    recorded = read_json(directory / "unit-result.json")
    require(recorded.get("Unit") == unit and unit_finished(recorded), "invalid_retained_unit_state")
    return dict(recorded, Retained=True)


def verify_source(request: dict[str, object]) -> Path:
    """Bind execution to an unchanged exact committed checkout on this host."""
    revision = text(request["revision"])
    source = ROOT / "sources" / revision
    protected(source, directory=True, private=False)
    require(
        command(["/usr/bin/git", "-C", str(source), "rev-parse", "HEAD"]) == revision,
        "source_revision_mismatch",
    )
    require(
        not command(
            ["/usr/bin/git", "-C", str(source), "status", "--porcelain=v1", "--untracked-files=all"]
        ),
        "remote_checkout_dirty",
    )
    return source


def images_for(request: dict[str, object]) -> dict[str, object]:
    """Verify immutable local image identities, revision labels and runtime users."""
    path = ROOT / "artifacts" / text(request["revision"]) / "images.json"
    protected(path)
    manifest = read_json(path)
    require(
        manifest.get("schemaVersion") == 1
        and manifest.get("passed") is True
        and manifest.get("revision") == request["revision"],
        "image_manifest_mismatch",
    )
    for role in ("server", "generator"):
        image = text(manifest.get(role + "Image"))
        require(re.fullmatch(r"sha256:[a-f0-9]{64}", image), "mutable_image_refused")
        inspected = array(cast("object", json.loads(command([*DOCKER, "image", "inspect", image]))))
        require(len(inspected) == 1, "invalid_image_inspection")
        details = obj(inspected[0])
        config = obj(details.get("Config"))
        require(
            details.get("Id") == image and config.get("User") == "10001:10001",
            "image_identity_mismatch",
        )
        require(
            obj(config.get("Labels")).get("org.opencontainers.image.revision")
            == request["revision"],
            "image_revision_mismatch",
        )
    return manifest


def capacity_argv(request: dict[str, object], manifest: dict[str, object]) -> list[str]:
    """Build an allowlisted capacity invocation, with no arbitrary env or shell input."""
    source = ROOT / "sources" / text(request["revision"])
    argv = [
        "/usr/bin/python3",
        "-B",
        str(source / "build/capacity.py"),
        "run",
        "--engine",
        "docker",
        "--output",
        str(evidence_path(request) / "workload"),
    ]
    mapping = {"serverImage": "--server-image", "generatorImage": "--generator-image"}
    for name, flag in mapping.items():
        argv.extend([flag, text(manifest[name])])
    for name, flag in {
        "label": "--label",
        "workload": "--workloads",
        "firstSize": "--first-size",
        "steps": "--steps",
        "meetingSize": "--meeting-size",
        "speakers": "--speakers",
        "chatIntervalMs": "--chat-interval-ms",
        "serverCpus": "--server-cpus",
        "generatorCpus": "--generator-cpus",
        "appCpus": "--app-cpus",
        "portMbps": "--port-mbps",
        "monthlyPrice": "--monthly-price",
    }.items():
        if request.get(name) is not None:
            argv.extend([flag, str(request[name])])
    for name, flag in (("audioOnly", "--audio-only"), ("quick", "--quick")):
        if request[name]:
            argv.append(flag)
    if request["workload"] == "meetings":
        for name in ("MAX_PARTICIPANTS_PER_ROOM", "MAX_BROADCASTERS_PER_ROOM"):
            argv.extend(["--server-env", f"{name}={request['meetingSize']}"])
    return argv


def assess(report: dict[str, object], request: dict[str, object]) -> dict[str, object]:
    """Separate passing lower bounds from failed/invalid workload observations."""
    require(report.get("schemaVersion") == 1, "invalid_calibration_schema")
    measurement = obj(report.get("measurement"))
    browser = obj(measurement.get("browser"))
    require(measurement.get("meetingSize") == request["meetingSize"], "measurement_shape_mismatch")
    require(
        all(
            browser.get(name) == request[name]
            for name in ("audioOnly", "speakers", "chatIntervalMs")
        ),
        "measurement_shape_mismatch",
    )
    for role in ("server", "generator"):
        image = obj(obj(report.get("images")).get(role))
        require(
            image.get("revision") == request["revision"]
            and re.fullmatch(r"sha256:[a-f0-9]{64}", text(image.get("id"))),
            "measurement_image_mismatch",
        )
    steps = [obj(item) for item in array(report.get("steps"))]
    require(
        steps and all(item.get("workload") == request["workload"] for item in steps),
        "missing_requested_workload",
    )
    require(
        all(type(item.get("valid")) is bool and type(item.get("passed")) is bool for item in steps),
        "invalid_step_verdict",
    )
    passing = [item for item in steps if item["valid"] is True and item["passed"] is True]
    failures = [item for item in steps if item["valid"] is not True or item["passed"] is not True]
    return {
        "passed": not failures,
        "passingSteps": passing,
        "failedSteps": failures,
        "ceilings": report.get("ceilings"),
        "projection": report.get("projection"),
    }


def residuals(directory: Path, before: list[str]) -> dict[str, object]:
    """Inspect only exact owned/new IDs and preserve uncertain evidence without removal."""
    after = container_ids()
    ownership = directory / "workload/ownership.json"
    expected: dict[str, dict[str, object]] = {}
    owned: list[str] = []
    if ownership.exists():
        journal = read_json(ownership)
        run = text(journal.get("runId"))
        require(re.fullmatch(r"[a-f0-9]{16}", run), "invalid_ownership_run")
        expected = {
            text(obj(item).get("name")): obj(item) for item in array(journal.get("containers"))
        }
        owned = container_ids(label=f"simplestchat.capacity.run={run}")
    discovered = sorted(set(after) - set(before) | set(owned))
    details: list[object] = []
    for identity in discovered:
        inspected = array(cast("object", json.loads(command([*DOCKER, "inspect", identity]))))
        require(
            len(inspected) == 1 and obj(inspected[0]).get("Id") == identity,
            "container_inspection_mismatch",
        )
        value = obj(inspected[0])
        name = text(value.get("Name")).removeprefix("/")
        match = expected.get(name)
        config = obj(value.get("Config"))
        confirmed = (
            identity in owned
            and match is not None
            and config.get("Image") == match.get("image")
            and match.get("id") in (None, identity)
        )
        state = obj(value.get("State"))
        details.append(
            {
                "id": identity,
                "confirmedOwned": confirmed,
                "inspection": {
                    "Id": identity,
                    "Name": name,
                    "Image": value.get("Image"),
                    "RequestedImage": config.get("Image"),
                    "State": {
                        key: state.get(key)
                        for key in (
                            "Status",
                            "Running",
                            "ExitCode",
                            "OOMKilled",
                            "StartedAt",
                            "FinishedAt",
                        )
                    },
                },
            }
        )
    return {
        "containersBefore": before,
        "containersAfter": after,
        "residuals": details,
        "clean": not discovered,
    }


def run_worker(request: dict[str, object]) -> int:  # noqa: PLR0915 -- one journaled workload transaction, including cleanup on every exit.
    """Execute under inherited flock; SIGTERM forwards to the one owned capacity child."""
    request = validate_request(request)
    directory = evidence_path(request)
    outcome: dict[str, object] = {
        "schemaVersion": 1,
        "revision": request["revision"],
        "passed": False,
        "finalized": False,
    }
    before: list[str] = []
    try:
        # Our unit is already active. Other guards are checked by the starter;
        # this locked recheck closes the launch race without rejecting ourselves.
        _ = preflight(allowed_unit=f"simplestchat-capacity-{request['run']}.service")
        current = STATE / "current.json"
        if current.exists():
            protected(current)
            require(read_json(current).get("finalized") is True, "cleanup_unfinished")
        _ = verify_source(request)
        manifest = images_for(request)
        before = container_ids()
        save(directory / "containers-before.json", {"ids": before})
        save(
            STATE / "current.json",
            {
                "schemaVersion": 1,
                "operation": "capacity",
                "finalized": False,
                "run": request["run"],
                "evidence": str(directory),
            },
        )
        save(directory / "images.json", manifest)
        argv = capacity_argv(request, manifest)
        save(directory / "command.json", {"argv": argv})
        docker_config = directory / "docker-config"
        docker_config.mkdir(mode=0o700)
        with (directory / "capacity.log").open("xb") as log:
            child = subprocess.Popen(  # noqa: S603 -- validated explicit argv.
                argv,
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=subprocess.STDOUT,
                env=dict(
                    ENVIRONMENT,
                    DOCKER_HOST="unix:///var/run/docker.sock",
                    DOCKER_CONFIG=str(docker_config),
                ),
            )

            def terminate(_signal: int, _frame: FrameType | None) -> None:
                if child.poll() is None:
                    child.terminate()

            previous = signal.signal(signal.SIGTERM, terminate)
            try:
                outcome["exitStatus"] = child.wait()
            finally:
                _ = signal.signal(signal.SIGTERM, previous)
        report = read_json(directory / "workload/calibration.json")
        verdict = assess(report, request)
        outcome.update(verdict)
        outcome["passed"] = outcome["exitStatus"] == 0 and verdict["passed"] is True
    except (CapacityControlError, OSError, ValueError, subprocess.SubprocessError) as error:
        outcome["failureClass"] = (
            str(error) if isinstance(error, CapacityControlError) else type(error).__name__
        )
    finally:
        try:
            cleanup = residuals(directory, before)
            outcome["cleanup"] = cleanup
            outcome["finalized"] = cleanup["clean"] is True
            outcome["passed"] = outcome["passed"] is True and outcome["finalized"] is True
            save(directory / "outcome.json", outcome)
            current = STATE / "current.json"
            if current.exists() and read_json(current).get("run") == request["run"]:
                save(
                    current,
                    {
                        "schemaVersion": 1,
                        "operation": "capacity",
                        "run": request["run"],
                        "evidence": str(directory),
                        "finalized": outcome["finalized"],
                        "passed": outcome["passed"],
                    },
                )
        except (CapacityControlError, OSError, ValueError, subprocess.SubprocessError):
            outcome["passed"] = outcome["finalized"] = False
            outcome["cleanupFailure"] = True
            save(directory / "outcome.json", outcome)
    return 0 if outcome["passed"] is True else 1


def start(request: dict[str, object], helper: str) -> dict[str, object]:
    """Start one bounded unit with an exact private request file; never retry it."""
    request = validate_request(request)
    with workload_lock():
        _ = preflight()
        source = verify_source(request)
        require(
            (source / "build/vps_capacity_remote.py").read_text() == helper,
            "helper_revision_mismatch",
        )
        _ = images_for(request)
        directory = evidence_path(request)
        directory.mkdir(mode=0o700)
        save(directory / "request.json", request)
        helper_path = directory / "worker.py"
        _ = helper_path.write_text(helper, encoding="utf-8")
        helper_path.chmod(0o600)
    unit = f"simplestchat-capacity-{request['run']}.service"
    argv = [
        "/usr/bin/systemd-run",
        "--quiet",
        "--unit",
        unit,
        "--property=Type=exec",
        "--property=RemainAfterExit=yes",
        f"--property=RuntimeMaxSec={request['runtimeSeconds']}",
        "--property=TimeoutStopSec=600",
        "--property=KillMode=mixed",
        "--property=Restart=no",
        "--property=UMask=0077",
        "--property=LimitCORE=0",
        "/usr/bin/flock",
        "--nonblock",
        "--no-fork",
        str(STATE / "workload.lock"),
        "/usr/bin/python3",
        "-I",
        str(helper_path),
        str(directory / "request.json"),
    ]
    save(directory / "unit-command.json", {"argv": argv})
    _ = command(argv)
    return {"unit": unit, "evidence": str(directory), "run": request["run"]}


def build_images(request: dict[str, object]) -> dict[str, object]:
    """Start only the canonical, prepared, bounded image-build service."""
    with workload_lock():
        _ = preflight()
        source = verify_source(request)
        launcher = Path("/usr/local/libexec/simplestchat-bench/build-images")
        protected(launcher, private=False)
        script = launcher.read_text()
        require(
            f"revision='{request['revision']}'" in script and f"source_root='{source}'" in script,
            "image_launcher_revision_mismatch",
        )
        unit = Path("/etc/systemd/system") / BUILD_UNIT
        protected(unit, private=False)
        require(
            unit.read_text()
            == (source / "ops/ansible/templates/image-build.service.j2").read_text(),
            "image_unit_mismatch",
        )
    _ = command(["/usr/bin/systemctl", "start", BUILD_UNIT])
    return {"unit": BUILD_UNIT}


def collect(request: dict[str, object]) -> dict[str, object]:
    """Retain final unit, journal and residual evidence including forced termination."""
    directory = evidence_path(request)
    protected(directory, directory=True)
    saved = read_json(directory / "request.json")
    require(saved.get("revision") == request.get("revision"), "collection_revision_mismatch")
    unit = f"simplestchat-capacity-{request['run']}.service"
    state = capacity_unit_state(request)
    require(unit_finished(state), "capacity_still_running")
    state["Unit"] = unit
    save(directory / "unit-result.json", state)
    journal = command(
        ["/usr/bin/journalctl", "--unit", unit, "--no-pager", "--lines=2000", "--output=short-iso"]
    )
    _ = (directory / "unit.log").write_text(journal, encoding="utf-8")
    with workload_lock():
        before_file = directory / "containers-before.json"
        before = (
            [text(item) for item in array(read_json(before_file).get("ids"))]
            if before_file.exists()
            else []
        )
        cleanup = residuals(directory, before)
        save(directory / "collection-cleanup.json", cleanup)
    outcome_file = directory / "outcome.json"
    outcome = (
        read_json(outcome_file)
        if outcome_file.exists()
        else {"passed": False, "finalized": False, "failureClass": "worker_outcome_missing"}
    )
    passed = (
        outcome.get("passed") is True
        and cleanup["clean"] is True
        and state.get("Result") == "success"
        and state.get("ExecMainStatus") == "0"
    )
    result = {
        "passed": passed,
        "unit": state,
        "outcome": outcome,
        "cleanup": cleanup,
        "evidence": str(directory),
        "revision": saved["revision"],
        "request": saved,
    }
    save(directory / "collection.json", result)
    # RemainAfterExit keeps success inspectable across fresh SSH connections.
    # Release that exact completed unit only after its result is durably saved;
    # a repeated collection uses the retained record after systemd unloads it.
    if state.get("ActiveState") == "active" and state.get("Retained") is not True:
        _ = command(["/usr/bin/systemctl", "stop", unit])
    return result


def recover(request: dict[str, object]) -> dict[str, object]:
    """Explicitly remove proven owned residual IDs, retaining all ambiguous resources.

    A recovery never reruns the workload or converts a failed measurement to a
    pass. Its receipt is separate, and cleanup is re-inspected before unblocking
    the canonical journal for a future explicit run.
    """
    directory = evidence_path(request)
    protected(directory, directory=True)
    saved = read_json(directory / "request.json")
    require(saved.get("revision") == request.get("revision"), "collection_revision_mismatch")
    require(not Path("/etc/simplestchat-public/compose.public.yml").exists(), "public_host_refused")
    state = capacity_unit_state(request)
    require(unit_finished(state), "capacity_still_running")
    receipt: dict[str, object] = {"run": request["run"], "removed": [], "finalized": False}
    removed: list[str] = []
    with workload_lock():
        before = [
            text(item) for item in array(read_json(directory / "containers-before.json").get("ids"))
        ]
        try:
            snapshot = residuals(directory, before)
            receipt["before"] = snapshot
            candidates = array(snapshot["residuals"])
            require(
                len(candidates) <= MAX_RECOVERY_CONTAINERS,
                "too_many_residuals_for_bounded_recovery",
            )
            # Names choose dependency order only after each destructive action
            # separately proves the immutable ID's ownership below.
            candidates.sort(
                key=lambda item: (
                    not text(obj(obj(item).get("inspection", {})).get("Name", "")).startswith(
                        "capacity-gen-"
                    )
                )
            )
            for candidate in candidates:
                value = obj(candidate)
                if value.get("confirmedOwned") is not True:
                    continue
                identity = text(value["id"])
                # Re-evaluate the exact ID, current label, expected name and image
                # immediately before its destructive command, without name lookup.
                current = [obj(item) for item in array(residuals(directory, before)["residuals"])]
                require(
                    any(
                        item.get("id") == identity and item.get("confirmedOwned") is True
                        for item in current
                    ),
                    "recovery_identity_changed",
                )
                _ = command([*DOCKER, "stop", "--time", "20", identity], timeout=30)
                _ = command([*DOCKER, "rm", identity], timeout=30)
                removed.append(identity)
            cleanup = residuals(directory, before)
            receipt.update(removed=removed, after=cleanup)
            journal_path = STATE / "current.json"
            if cleanup["clean"] is True and journal_path.exists():
                protected(journal_path)
                journal = read_json(journal_path)
                require(journal.get("run") == request["run"], "recovery_journal_owner_changed")
                save(journal_path, dict(journal, finalized=True, passed=False, recovered=True))
            receipt["finalized"] = cleanup["clean"]
        finally:
            receipt["removed"] = removed
            save(directory / "recovery.json", receipt)
    return receipt


def stream_archive(request: dict[str, object]) -> None:
    """Stream only bounded regular evidence files; never follow links or devices."""
    directory = evidence_path(request)
    protected(directory, directory=True)
    require(
        read_json(directory / "request.json").get("revision") == request.get("revision"),
        "collection_revision_mismatch",
    )
    require((directory / "collection.json").is_file(), "collection_required")
    paths = sorted(directory.rglob("*"))
    require(len(paths) <= MAX_FILES, "too_many_artifacts")
    total = 0
    with tempfile.TemporaryFile() as stream:
        with tarfile.open(fileobj=stream, mode="w:gz") as archive:
            for path in paths:
                info = path.lstat()
                require(
                    stat.S_ISDIR(info.st_mode) or stat.S_ISREG(info.st_mode), "unsafe_artifact_kind"
                )
                if path.is_dir():
                    continue
                require(info.st_size <= MAX_FILE, "artifact_too_large")
                total += info.st_size
                require(total <= MAX_EXPANDED, "artifacts_too_large")
                archive.add(path, arcname=path.relative_to(directory).as_posix(), recursive=False)
        require(stream.tell() <= MAX_ARCHIVE, "archive_too_large")
        _ = stream.seek(0)
        while chunk := stream.read(1024**2):
            _ = sys.stdout.buffer.write(chunk)
        _ = sys.stdout.buffer.flush()


def serve(request: dict[str, object], helper: str) -> None:
    """Dispatch the allowlisted transport operations; no remote CLI interpolation."""
    _ = os.umask(0o077)
    action = request.get("action")
    result: dict[str, object]
    if action == "reserve":
        with workload_lock():
            result = preflight()
            print(json.dumps(result), flush=True)  # noqa: T201 -- one private protocol record.
            require(select.select([sys.stdin], [], [], 1800)[0], "preparation_lease_expired")
            require(sys.stdin.readline(32).strip() == "release", "preparation_lease_lost")
        return
    if action == "preflight":
        with workload_lock():
            result = preflight()
    elif action == "build":
        result = build_images(validate_request(request))
    elif action == "start":
        result = start(request, helper)
    elif action == "status":
        result = (
            unit_state(BUILD_UNIT)
            if request.get("build") is True
            else capacity_unit_state(validate_request(request))
        )
    elif action == "collect":
        result = collect(validate_request(request))
    elif action == "recover":
        result = recover(validate_request(request))
    elif action == "archive":
        stream_archive(validate_request(request))
        return
    else:
        raise CapacityControlError("unknown_operation")
    print(json.dumps(result), flush=True)  # noqa: T201 -- private protocol response.


if __name__ == "__main__":
    raise SystemExit(run_worker(read_json(Path(sys.argv[1]))))
