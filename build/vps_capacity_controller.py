"""Run and collect a private VPS capacity experiment from exact committed source.

Only one private benchmark inventory host is selected. Optional preparation
updates source and opt-in units; optional builds use the canonical service.
SSH carries validated JSON on stdin, never user-selected remote shell text.
An uncertain run is retained and may be collected or explicitly recovered by ID.
"""

# Failure codes deliberately avoid printing private inventory/subprocess output.
# ruff: noqa: EM101

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import selectors
import shlex
import subprocess
import sys
import tarfile
import time
import uuid
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path, PurePosixPath
from typing import TYPE_CHECKING, cast

import bootstrap_controller as BOOTSTRAP  # noqa: N812 -- shared controller convention.
import release_build as BUILD  # noqa: N812 -- shared controller convention.
import release_fetch_controller as FETCH  # noqa: N812 -- shared strict SSH environment.
import vps_capacity_remote as REMOTE  # noqa: N812 -- shared validated protocol.
from bootstrap_access import BootstrapError

if TYPE_CHECKING:
    from collections.abc import Generator

ROOT = Path(__file__).resolve().parents[1]
MAX_REQUEST = 512 * 1024
LOADER = (
    "import json,sys; "
    + "envelope=json.loads(sys.stdin.buffer.readline(524289)); "
    + "scope={'__name__':'capacity_transport'}; "
    + "exec(compile(envelope['helper'],'committed-capacity-helper','exec'),scope); "
    + "scope['serve'](envelope['request'],envelope['helper'])"
)


@dataclass
class Options(argparse.Namespace):
    """Explicit target, identity, bounded workload and recovery selectors."""

    inventory: str = ""
    limit: str = ""
    revision: str = ""
    label: str = "private-vps"
    prepare: bool = False
    build_images: bool = False
    collect: str | None = None
    recover: str | None = None
    workload: str = "meetings"
    first_size: int = 30
    steps: int = 1
    meeting_size: int = 30
    speakers: int = 30
    audio_only: bool = False
    chat_interval_ms: int = 0
    quick: bool = False
    runtime_seconds: int = 7200
    monthly_price: float | None = None
    server_cpus: float | None = None
    generator_cpus: float | None = None
    app_cpus: float | None = None
    port_mbps: float = 1000
    validated_inventory: dict[str, object] | None = None


def workload_request(args: Options, run: str) -> dict[str, object]:
    """Use a single validation contract on both sides of the SSH boundary."""
    return REMOTE.validate_request(
        {
            "revision": args.revision,
            "run": run,
            "label": args.label,
            "workload": args.workload,
            "firstSize": args.first_size,
            "steps": args.steps,
            "meetingSize": args.meeting_size,
            "speakers": args.speakers,
            "audioOnly": args.audio_only,
            "chatIntervalMs": args.chat_interval_ms,
            "quick": args.quick,
            "runtimeSeconds": args.runtime_seconds,
            "monthlyPrice": args.monthly_price,
            "serverCpus": args.server_cpus,
            "generatorCpus": args.generator_cpus,
            "appCpus": args.app_cpus,
            "portMbps": args.port_mbps,
        }
    )


def options(argv: list[str] | None = None) -> Options:
    """Parse bounded selectors before inventory, transport or output side effects."""
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("inventory", "limit", "revision"):
        _ = parser.add_argument("--" + name, required=True)
    for name in ("prepare", "build-images", "audio-only", "quick"):
        _ = parser.add_argument("--" + name, action="store_true")
    _ = parser.add_argument("--label", default="private-vps")
    recovery = parser.add_mutually_exclusive_group()
    _ = recovery.add_argument("--collect", metavar="RUN_ID")
    _ = recovery.add_argument("--recover", metavar="RUN_ID")
    _ = parser.add_argument(
        "--workload", choices=("meetings", "large-meeting", "webinar"), default="meetings"
    )
    for name, default in (
        ("first-size", 30),
        ("steps", 1),
        ("meeting-size", 30),
        ("speakers", 30),
        ("chat-interval-ms", 0),
        ("runtime-seconds", 7200),
    ):
        _ = parser.add_argument("--" + name, type=int, default=default)
    for name in ("monthly-price", "server-cpus", "generator-cpus", "app-cpus"):
        _ = parser.add_argument("--" + name, type=float)
    _ = parser.add_argument("--port-mbps", type=float, default=1000)
    args = parser.parse_args(argv, namespace=Options())
    REMOTE.require(re.fullmatch(r"[A-Za-z][A-Za-z0-9_-]{0,62}", args.limit), "invalid_exact_limit")
    REMOTE.require(
        not ((args.collect or args.recover) and (args.prepare or args.build_images)),
        "recovery_cannot_start_work",
    )
    _ = workload_request(args, args.collect or args.recover or "0" * 32)
    args.inventory = str(BOOTSTRAP.local_path(args.inventory))
    REMOTE.require(Path(args.inventory).is_file(), "invalid_inventory")
    return args


def inventory_target(args: Options, root: Path) -> BOOTSTRAP.BootstrapTarget:
    """Validate one protected static host; never invoke executable Ansible inventory."""
    variables = BOOTSTRAP.inventory_host(Path(args.inventory), args.limit)
    allowed_transport = {
        "ansible_host",
        "ansible_user",
        "ansible_port",
        "ansible_connection",
        "ansible_ssh_private_key_file",
        "ansible_ssh_common_args",
    }
    REMOTE.require(
        all(not key.startswith("ansible_") or key in allowed_transport for key in variables),
        "unsupported_inventory_transport_override",
    )
    REMOTE.require(variables.get("scpub_enabled", False) is False, "public_inventory_refused")
    REMOTE.require(variables.get("ansible_connection", "ssh") == "ssh", "ssh_inventory_required")
    REMOTE.require(
        variables.get("scbench_root", str(REMOTE.ROOT)) == str(REMOTE.ROOT),
        "noncanonical_remote_root",
    )
    host = REMOTE.text(variables.get("ansible_host", args.limit))
    user = REMOTE.text(variables.get("ansible_user"))
    port = REMOTE.integer(variables.get("ansible_port", 22), 1, 65535)
    REMOTE.require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9.:-]{0,252}", host), "invalid_ssh_host")
    REMOTE.require(re.fullmatch(r"[a-z_][a-z0-9_-]{0,31}", user), "invalid_ssh_user")
    identity = BOOTSTRAP.local_path(REMOTE.text(variables.get("ansible_ssh_private_key_file")))
    known_hosts = BOOTSTRAP.local_path(
        REMOTE.text(
            variables.get("scbench_ssh_known_hosts_file", str(Path.home() / ".ssh/known_hosts"))
        )
    )
    BOOTSTRAP.outside_checkout(identity, root)
    BOOTSTRAP.private_parent(identity)
    BOOTSTRAP.private_parent(known_hosts)
    _ = BOOTSTRAP.protected_file(identity)
    _ = BOOTSTRAP.protected_file(known_hosts, secret=False)
    target = BOOTSTRAP.BootstrapTarget(
        host=host, user=user, port=port, identity=identity, known_hosts=known_hosts
    )
    REMOTE.require(
        variables.get("ansible_ssh_common_args", "")
        in ("", BOOTSTRAP.inventory_ssh_common_args(target)),
        "unsupported_inventory_ssh_args",
    )
    args.validated_inventory = {"benchmark_hosts": {"hosts": {args.limit: variables}}}
    return target


@dataclass
class Transport:
    """One strict SSH target carrying only committed helper code and JSON data."""

    target: BOOTSTRAP.BootstrapTarget
    helper: str
    evidence: Path
    sequence: int = 0

    def argv(self) -> list[str]:
        """Select the fixed remote loader; no request value enters this command."""
        command = ["/usr/bin/python3", "-I", "-c", LOADER]
        if self.target.user != "root":
            command = ["sudo", "-n", "--", *command]
        return BOOTSTRAP.ssh_command(self.target, shlex.join(command))

    def payload(self, request: dict[str, object]) -> bytes:
        """Bound the entire helper/request envelope before sending it."""
        data = (json.dumps({"helper": self.helper, "request": request}) + "\n").encode()
        REMOTE.require(len(data) <= MAX_REQUEST, "remote_request_too_large")
        return data

    def call(self, request: dict[str, object], *, timeout: int = 60) -> dict[str, object]:
        """Run one bounded operation and preserve raw output privately on failure."""
        self.sequence += 1
        stem = f"{self.sequence:03d}-{request['action']}"
        with (
            (self.evidence / f"{stem}.stdout").open("xb") as output,
            (self.evidence / f"{stem}.stderr").open("xb") as error,
        ):
            child = subprocess.Popen(  # noqa: S603 -- fixed SSH loader and validated target.
                self.argv(),
                stdin=subprocess.PIPE,
                stdout=output,
                stderr=error,
                env=FETCH.ssh_environment(),
                start_new_session=True,
            )
            try:
                _ = child.communicate(self.payload(request), timeout=timeout)
                REMOTE.require(child.returncode == 0, "remote_operation_failed")
            finally:
                if child.poll() is None:
                    _ = BUILD.Runner.stop_process(child)
        path = self.evidence / f"{stem}.stdout"
        REMOTE.require(path.stat().st_size <= REMOTE.MAX_JSON, "remote_response_too_large")
        return REMOTE.obj(cast("object", json.loads(path.read_bytes())))

    @contextmanager
    def reserve(self, request: dict[str, object]) -> Generator[None]:
        """Hold the remote workload lock continuously across source preparation."""
        with (self.evidence / "preparation-lease.stderr").open("xb") as error:
            child: subprocess.Popen[bytes] = subprocess.Popen(  # noqa: S603 -- fixed SSH loader.
                self.argv(),
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=error,
                env=FETCH.ssh_environment(),
                start_new_session=True,
            )
            try:
                REMOTE.require(
                    child.stdin is not None and child.stdout is not None, "ssh_pipes_missing"
                )
                if child.stdin is None or child.stdout is None:
                    raise REMOTE.CapacityControlError("ssh_pipes_missing")
                _ = child.stdin.write(self.payload(dict(request, action="reserve")))
                child.stdin.flush()
                with selectors.DefaultSelector() as selector:
                    _ = selector.register(child.stdout, selectors.EVENT_READ)
                    REMOTE.require(selector.select(60), "preparation_lease_timeout")
                    record = cast("bytes", child.stdout.readline(REMOTE.MAX_JSON + 1))
                REMOTE.require(
                    len(record) <= REMOTE.MAX_JSON
                    and REMOTE.obj(cast("object", json.loads(record))).get("privateHost") is True,
                    "invalid_preparation_lease",
                )
                yield
                _ = child.stdin.write(b"release\n")
                child.stdin.flush()
                child.stdin.close()
                REMOTE.require(child.wait(timeout=30) == 0, "preparation_lease_lost")
            finally:
                if child.poll() is None:
                    _ = BUILD.Runner.stop_process(child)
                if child.stdin is not None:
                    child.stdin.close()
                if child.stdout is not None:
                    child.stdout.close()

    def download(self, request: dict[str, object], destination: Path) -> None:
        """Stream a bounded private archive without buffering it in memory."""
        with (
            destination.open("xb") as output,
            (self.evidence / "archive.stderr").open("xb") as error,
        ):
            child = subprocess.Popen(  # noqa: S603 -- fixed SSH loader.
                self.argv(),
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=error,
                env=FETCH.ssh_environment(),
                start_new_session=True,
            )
            try:
                if child.stdin is None or child.stdout is None:
                    raise REMOTE.CapacityControlError("ssh_pipes_missing")
                _ = child.stdin.write(self.payload(dict(request, action="archive")))
                child.stdin.close()
                deadline = time.monotonic() + 300
                size = 0
                with selectors.DefaultSelector() as selector:
                    _ = selector.register(child.stdout, selectors.EVENT_READ)
                    while True:
                        remaining = deadline - time.monotonic()
                        REMOTE.require(
                            remaining > 0 and selector.select(remaining), "archive_download_timeout"
                        )
                        chunk = os.read(child.stdout.fileno(), 1024**2)
                        if not chunk:
                            break
                        size += len(chunk)
                        REMOTE.require(size <= REMOTE.MAX_ARCHIVE, "archive_too_large")
                        _ = output.write(chunk)
                REMOTE.require(child.wait(timeout=15) == 0, "archive_download_failed")
            finally:
                if child.poll() is None:
                    _ = BUILD.Runner.stop_process(child)
                if child.stdout is not None:
                    child.stdout.close()


def extract_archive(archive: Path, destination: Path) -> None:
    """Extract new regular files only, enforcing paths, counts and expanded sizes."""
    REMOTE.require(archive.stat().st_size <= REMOTE.MAX_ARCHIVE, "archive_too_large")
    destination.mkdir(mode=0o700)
    seen: set[str] = set()
    total = 0
    with tarfile.open(archive, mode="r:gz") as bundle:
        for member in bundle:
            name = PurePosixPath(member.name)
            REMOTE.require(
                member.isfile()
                and not name.is_absolute()
                and name.parts
                and all(part not in ("", ".", "..") for part in name.parts)
                and "\\" not in member.name,
                "unsafe_archive_entry",
            )
            REMOTE.require(
                member.name not in seen and len(seen) < REMOTE.MAX_FILES,
                "duplicate_or_excess_archive_entries",
            )
            REMOTE.require(0 <= member.size <= REMOTE.MAX_FILE, "artifact_too_large")
            seen.add(member.name)
            total += member.size
            REMOTE.require(total <= REMOTE.MAX_EXPANDED, "artifacts_too_large")
            target = destination.joinpath(*name.parts)
            target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            source = bundle.extractfile(member)
            REMOTE.require(source is not None, "missing_archive_body")
            if source is None:
                raise REMOTE.CapacityControlError("missing_archive_body")
            with source, target.open("xb") as output:
                remaining = member.size
                while remaining:
                    chunk = source.read(min(1024**2, remaining))
                    REMOTE.require(chunk, "truncated_archive_body")
                    _ = output.write(chunk)
                    remaining -= len(chunk)
            target.chmod(0o600)


def wait_for_unit(
    transport: Transport, request: dict[str, object], *, build: bool = False
) -> dict[str, object]:
    """Observe one bounded unit; do not restart it or retry failed SSH operations."""
    deadline = time.monotonic() + (
        6300 + 90 + 60 if build else int(REMOTE.number(request["runtimeSeconds"], 120, 21600)) + 660
    )
    while True:
        result = transport.call(dict(request, action="status", build=build))
        if REMOTE.unit_finished(result):
            return result
        REMOTE.require(time.monotonic() < deadline, "unit_wait_expired_collect_before_retry")
        time.sleep(5)


def execute(args: Options, root: Path = ROOT) -> dict[str, object]:  # noqa: PLR0915 -- single bounded preparation, execution and collection transaction.
    """Prepare/build/run once, then retain verdict and evidence even for failed trials."""
    revision = BUILD.clean_revision(BUILD.Runner(), root)
    REMOTE.require(revision == args.revision, "controller_revision_mismatch")
    target = inventory_target(args, root)
    run = args.collect or args.recover or uuid.uuid4().hex
    request = workload_request(args, run)
    parent = root / "results"
    parent.mkdir(mode=0o700, exist_ok=True)
    output = BUILD.validate_output(
        parent / f"vps-capacity.{datetime.now(UTC):%Y%m%dT%H%M%SZ}.{uuid.uuid4().hex[:8]}",
        root,
        BUILD.Runner(),
    )
    output.mkdir(mode=0o700)
    helper = (root / "build/vps_capacity_remote.py").read_text(encoding="utf-8")
    transport = Transport(target, helper, output)
    report: dict[str, object] = {
        "schemaVersion": 1,
        "passed": False,
        "revision": revision,
        "run": run,
        "inventoryHost": args.limit,
        "evidence": str(output),
        "request": request,
        "remoteOutcome": "not_started",
    }
    REMOTE.save(output / "report.json", report)
    try:
        if not args.collect and not args.recover:
            _ = transport.call(dict(request, action="preflight"))
            if args.prepare:
                REMOTE.require(args.validated_inventory is not None, "inventory_not_validated")
                inventory_snapshot = output / "inventory.snapshot.json"
                REMOTE.save(inventory_snapshot, REMOTE.obj(args.validated_inventory))
                with transport.reserve(request):
                    argv = BOOTSTRAP.site_command(root, inventory_snapshot, args.limit, revision)
                    argv.extend(["--tags", "source,benchmark"])
                    argv.extend(
                        [
                            "--extra-vars",
                            json.dumps(
                                {
                                    "ansible_ssh_common_args": BOOTSTRAP.inventory_ssh_common_args(
                                        target
                                    )
                                }
                            ),
                        ]
                    )
                    _, _ = BUILD.Runner(output=output).run(
                        argv,
                        cwd=root,
                        env=BOOTSTRAP.ansible_environment(root),
                        timeout=1500,
                        capture=False,
                    )
            if args.build_images:
                report["build"] = transport.call(dict(request, action="build"))
                result = wait_for_unit(transport, request, build=True)
                report["buildResult"] = result
                REMOTE.require(
                    result.get("Result") == "success" and result.get("ExecMainStatus") == "0",
                    "image_build_failed",
                )
            report["remoteOutcome"] = "start_requested"
            REMOTE.save(output / "report.json", report)
            report["start"] = transport.call(dict(request, action="start"))
        report["remoteOutcome"] = "observing"
        REMOTE.save(output / "report.json", report)
        report["unit"] = wait_for_unit(transport, request)
        if args.recover:
            report["recovery"] = transport.call(dict(request, action="recover"), timeout=600)
        result = transport.call(dict(request, action="collect"))
        report["result"] = result
        report["request"] = result.get("request", request)
        archive = output / "remote.tar.gz"
        transport.download(request, archive)
        extract_archive(archive, output / "remote")
        with archive.open("rb") as contents:
            report["archiveSha256"] = hashlib.file_digest(contents, "sha256").hexdigest()
        report["remoteOutcome"] = "collected"
        report["passed"] = result.get("passed") is True
    except (
        REMOTE.CapacityControlError,
        BUILD.BuildError,
        BootstrapError,
        OSError,
        ValueError,
        subprocess.SubprocessError,
        tarfile.TarError,
    ) as error:
        report["failureClass"] = (
            str(error) if isinstance(error, REMOTE.CapacityControlError) else type(error).__name__
        )
    finally:
        REMOTE.save(output / "report.json", report)
    return report


def main(argv: list[str] | None = None) -> int:
    """Return failure for invalid/failing workloads, preserving usable passing bounds."""
    _ = os.umask(0o077)
    try:
        result = execute(options(argv))
        print(json.dumps(result, indent=2))  # noqa: T201 -- explicit controller receipt.
        return 0 if result["passed"] is True else 1
    except (
        REMOTE.CapacityControlError,
        BUILD.BuildError,
        BootstrapError,
        OSError,
        ValueError,
    ) as error:
        print(f"capacity controller: {type(error).__name__}: {error}", file=sys.stderr)  # noqa: T201 -- deliberate CLI failure.
        return 1
