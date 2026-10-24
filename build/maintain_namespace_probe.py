"""TEST ONLY: own the fixed namespace catalog in one disposable Debian KVM guest.

There is no host, image, command, case or production-launcher selector. This
prototype performs no candidate authentication.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import math
import os
import re
import shutil
import signal
import socket
import sys
import tarfile
import tempfile
import time
import uuid
from pathlib import Path
from typing import TYPE_CHECKING, cast

# Isolated entry restores only this closed source projection's helper directory.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from security_context import ROOT

# isort: split
import maintain_namespace_probe_guest as probe
import security_vm as vm
import security_vm_ops as ops_vm
import security_vm_ops_guest as peers
from release_json import decode_json, object_value
from security_tools import ToolError, write_private

if TYPE_CHECKING:
    from collections.abc import Callable, Sequence
    from types import FrameType

BODY_SECONDS = 1680
FINAL_SECONDS = 120
MINIMUM_WORK = 100
NAMES = ("maintain_namespace_probe_guest.py", "bounded_process.py")
REMOTE = "/root/simplestchat-vm-fixture/maintain-namespace"
VERIFY = r"""
import hashlib,json,os,stat,sys
from pathlib import Path
root=Path(sys.argv[1]);pins=json.loads(sys.argv[2])
assert set(pins)=={"maintain_namespace_probe_guest.py","bounded_process.py"}
assert root.is_dir() and not root.is_symlink()
for name,expected in pins.items():
    fd=os.open(root/name,os.O_RDONLY|os.O_NOFOLLOW|os.O_CLOEXEC)
    try:
        before=os.fstat(fd)
        assert stat.S_ISREG(before.st_mode) and before.st_uid==0
        assert stat.S_IMODE(before.st_mode)==0o600 and before.st_nlink==1
        with os.fdopen(os.dup(fd),"rb") as source:
            actual=hashlib.file_digest(source,"sha256").hexdigest()
        assert actual==expected
        after=os.fstat(fd)
        old=(before.st_dev,before.st_ino,before.st_size,before.st_mtime_ns,before.st_ctime_ns)
        new=(after.st_dev,after.st_ino,after.st_size,after.st_mtime_ns,after.st_ctime_ns)
        assert old==new
    finally:os.close(fd)
print("source-bindings-checked")
"""

EXPERIMENT = "namespace"
CHECKPOINTS = frozenset(
    {
        "admission",
        "boot",
        "stage",
        "guest",
        "guest-decode",
        "inputs",
        "final-source",
        "body-completion",
        "cleanup",
        "restore",
        "summary-output",
        "final-fence",
    }
)
GUEST_PHASES: frozenset[str] = frozenset()
FAILED_CASES = frozenset(probe.CASES)


def metadata_require(value: object, _code: str) -> None:
    """Use the original fixed guest-policy error without copying a caller string."""
    peers.require(value)


def directory_stamp(value: os.stat_result) -> tuple[int, ...]:
    """Bind only the locally admitted output directory identity/ownership, not mutable contents."""
    return (value.st_dev, value.st_ino, value.st_mode, value.st_uid, value.st_gid)


EXCEPTION_KINDS = frozenset({"policy", "io", "timeout", "signal", "decode", "unexpected"})
MAX_FAILURE_RECORD = 4096
MAX_SIGNED_STATUS = 255
OBSERVED_STATUSES = tuple(range(-MAX_SIGNED_STATUS, MAX_SIGNED_STATUS + 1))


def exception_kind(error: BaseException | None) -> str | None:
    """Classify a fixed family without exposing any error text, class name or arguments."""
    if error is None:
        return None
    catalog = (
        (TimeoutError, "timeout"),
        (KeyboardInterrupt, "signal"),
        ((probe.ProbeError, peers.OpsError, ToolError), "policy"),
        ((ValueError, UnicodeError), "decode"),
        (OSError, "io"),
    )
    for family, kind in catalog:
        if isinstance(error, family):
            return kind
    return "unexpected"


def observe_failure(state: dict[str, object], error: BaseException, checkpoint: str) -> None:
    """Latch this actual first refusal before cleanup or later diagnostic output can fail."""
    if state.get("_first") is None:
        state["_first"] = error
        state["exception"] = exception_kind(error)
        state["failedCheckpoint"] = checkpoint


def first_error(state: dict[str, object], outer: BaseException | None) -> BaseException | None:
    """Keep a same-process exception owner; no guest JSON can grant this private state."""
    value = state.get("_first")
    return value if isinstance(value, BaseException) else outer


def enum_value(value: object, allowed: frozenset[str]) -> str | None:
    """Project only an explicitly cataloged scalar."""
    return value if isinstance(value, str) and value in allowed else None


def signed_status(value: object) -> int | None:
    """Keep an actually observed signed status distinct from bool, unknown and SDK defaults."""
    return (
        value if type(value) is int and -MAX_SIGNED_STATUS <= value <= MAX_SIGNED_STATUS else None
    )


def failure_summary(
    state: dict[str, object],
    report: dict[str, object] | None,
    outer: BaseException | None,
    *,
    restored: bool,
) -> dict[str, object]:
    """Build one bounded negative projection without copying raw/nested guest or error data."""
    row_hash = state.get("rowSha256")
    row_bytes = state.get("rowBytes")
    checkpoint = state.get("failedCheckpoint", state.get("checkpoint"))
    return {
        "schemaVersion": 1,
        "experiment": EXPERIMENT,
        "passed": False,
        "metadataOnly": True,
        "controllerCheckpoint": enum_value(checkpoint, CHECKPOINTS),
        "exception": enum_value(state.get("exception"), EXCEPTION_KINDS),
        "outerException": exception_kind(outer),
        "observedSshStatus": signed_status(state.get("sshStatus")),
        "guestCheckpoint": enum_value(state.get("guestCheckpoint"), GUEST_PHASES),
        "guestFailedCase": enum_value(state.get("guestFailedCase"), FAILED_CASES),
        "guestException": enum_value(state.get("guestException"), EXCEPTION_KINDS),
        "guestCleanupException": enum_value(state.get("guestCleanupException"), EXCEPTION_KINDS),
        "guestCleanupPassed": state.get("guestCleanupPassed")
        if type(state.get("guestCleanupPassed")) is bool
        else None,
        "guestRowAccepted": state.get("guestRowAccepted") is True,
        "rowSha256": row_hash
        if isinstance(row_hash, str) and re.fullmatch(r"[a-f0-9]{64}", row_hash)
        else None,
        "rowBytes": row_bytes
        if type(row_bytes) is int and 0 <= row_bytes <= vm.MAX_STREAM
        else None,
        "cleanupPassed": report.get("cleanupPassed")
        if report is not None and type(report.get("cleanupPassed")) is bool
        else state.get("cleanupPassed")
        if type(state.get("cleanupPassed")) is bool
        else None,
        "cleanupException": enum_value(state.get("cleanupException"), EXCEPTION_KINDS),
        "restorationPassed": restored,
        "finalFencePassed": state.get("finalFencePassed") is True,
        "controllerSignalStatus": signed_status(state.get("signalStatus")),
    }


def retain_failure(
    destination: tuple[Path, tuple[int, ...] | None, float],
    metadata: dict[str, object],
    primary: BaseException | None,
) -> BaseException | None:
    """Attempt only fixed closed metadata output; publication never changes the negative verdict."""
    raw = json.dumps(metadata, separators=(",", ":"), allow_nan=False).encode() + b"\n"
    metadata_require(len(raw) <= MAX_FAILURE_RECORD, "failure-summary-bound")
    output, original, end = destination

    def write() -> None:
        if original is not None:
            metadata_require(time.monotonic() < end, "failure-output-deadline")
            metadata_require(directory_stamp(output.lstat()) == original, "failure-output-changed")
            write_private(output / "failure-summary.json", raw, mode=0o600)

    def console() -> None:
        text = "maintenance-prerequisite-failure " + raw.decode()
        metadata_require(sys.stderr.write(text) == len(text), "failure-console-short")
        _ = sys.stderr.flush()

    return peers.settle((write, console), primary)


def source_inputs() -> dict[str, bytes]:
    """Read a fixed source closure, never discover inputs through Git or generated results."""
    return {
        NAMES[0]: (ROOT / "build" / NAMES[0]).read_bytes(),
        NAMES[1]: (ROOT / "ops/ansible/files" / NAMES[1]).read_bytes(),
    }


def stage(host: ops_vm.Host, work: Path, port: int, sources: dict[str, bytes]) -> None:
    """Transfer only two original inert files into one fresh private guest directory."""
    peers.require(set(sources) == set(NAMES))
    archive_path = work / "inert-source.tar"
    descriptor = os.open(
        archive_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600
    )
    with (
        os.fdopen(descriptor, "wb") as stream,
        tarfile.open(fileobj=stream, mode="w", format=tarfile.USTAR_FORMAT) as archive,
    ):
        for name in NAMES:
            data = sources[name]
            info = tarfile.TarInfo(name)
            info.mode, info.uid, info.gid, info.mtime = 0o600, 0, 0, 0
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
    _ = host.run(
        "probe-directory",
        vm.ssh_command(
            work,
            port,
            [
                "sudo",
                "-n",
                "/usr/bin/mkdir",
                "-m",
                "700",
                REMOTE,
            ],
        ),
    )
    with archive_path.open("rb") as archive_input:
        _ = host.run(
            "probe-source-transfer",
            vm.ssh_command(
                work,
                port,
                [
                    "sudo",
                    "-n",
                    "/usr/bin/tar",
                    "-xf",
                    "-",
                    "-C",
                    REMOTE,
                    "--no-same-owner",
                    "--no-same-permissions",
                ],
            ),
            input_file=archive_input,
        )
    pins = {name: hashlib.sha256(raw).hexdigest() for name, raw in sources.items()}
    _ = host.run(
        "probe-source-bindings",
        vm.ssh_command(
            work,
            port,
            [
                "sudo",
                "-n",
                "/usr/bin/python3",
                "-I",
                "-B",
                "-c",
                VERIFY,
                REMOTE,
                json.dumps(pins, separators=(",", ":")),
            ],
        ),
    )


def guest_failure_record(raw: bytes, run_id: str) -> dict[str, object]:
    """Keep only a closed fixed failed-case/exception projection from the original guest row."""
    result: dict[str, object] = {
        "rowBytes": len(raw),
        "rowSha256": hashlib.sha256(raw).hexdigest(),
        "guestRowAccepted": False,
    }
    try:
        peers.require(0 < len(raw) <= vm.MAX_STREAM)
        value = object_value(decode_json(raw))
        row = cast("dict[str, object]", value)
        peers.require(
            set(row)
            == {
                "schemaVersion",
                "scope",
                "runId",
                "passed",
                "failure",
                "exception",
                "outerSignalStatus",
                "tools",
                "finalToolsUnchanged",
                "packages",
                "distribution",
                "kernel",
                "cases",
                "actualLauncherExecuted",
            }
            and type(row["schemaVersion"]) is int
            and row["schemaVersion"] == 1
            and row["scope"] == "TestOnlyMaintainerNamespaceProbe"
            and row["runId"] == run_id
            and type(row["passed"]) is bool
            and isinstance(row["cases"], list)
        )
        rows = cast("list[object]", row["cases"])
        peers.require(len(rows) <= len(probe.CASES))
        failed: str | None = None
        for index, item in enumerate(rows):
            peers.require(isinstance(item, dict))
            case = cast("dict[str, object]", item)
            peers.require(
                case.get("name") == probe.CASES[index] and type(case.get("passed")) is bool
            )
            if case["passed"] is False and failed is None:
                failed = probe.CASES[index]
        kind = enum_value(row["exception"], EXCEPTION_KINDS)
        peers.require(row["exception"] is None or kind is not None)
        result.update({"guestRowAccepted": True, "guestFailedCase": failed, "guestException": kind})
    except (OSError, ValueError, peers.OpsError, RecursionError):
        pass
    return result


def observe_guest(
    status: int, raw: bytes, run_id: str, state: dict[str, object]
) -> dict[str, object]:
    """Latch an actual returned status before optional metadata; validate the body separately."""
    state["sshStatus"] = status
    refusal: BaseException | None = None
    if status != 0:
        message = "ops_guest_unproven"
        refusal = peers.OpsError(message)
        observe_failure(state, refusal, "guest")
    outcome = peers.settle((lambda: state.update(guest_failure_record(raw, run_id)),), refusal)
    if outcome is not None:
        raise outcome
    state["checkpoint"] = "guest-decode"
    peers.require(len(raw) <= vm.MAX_STREAM)
    observed = cast("object", json.loads(raw))
    peers.require(isinstance(observed, dict))
    return cast("dict[str, object]", observed)


def run(  # noqa: PLR0915 -- One original VM owner and finalization path.
    output: Path, control: peers.Control, diagnostic: dict[str, object] | None = None
) -> dict[str, object]:
    """Keep one original framework clock through boot, trials, actual QEMU0 and final closes."""
    state: dict[str, object] = {} if diagnostic is None else diagnostic
    state["checkpoint"] = "admission"
    peers.require(
        sys.platform == "linux"
        and output.is_absolute()
        and output.resolve() == output
        and not output.exists()
    )
    vm.preflight(output)
    sources = source_inputs()
    pin = vm.CloudImage.read()
    report: dict[str, object] = {
        "schemaVersion": 1,
        "scope": "TestOnlyMaintainerNamespaceProbeVM",
        "passed": False,
        "cleanupPassed": False,
        "actualLauncherExecuted": False,
        "cloudImageSha512": pin.sha512,
        "sourceSha256": {name: hashlib.sha256(raw).hexdigest() for name, raw in sources.items()},
    }
    first: BaseException | None = None
    guest: vm.Guest | None = None
    work: Path | None = None
    work_fd: int | None = None
    try:
        control.check()
        output.mkdir(mode=0o700)
        state["_outputStamp"] = directory_stamp(output.lstat())
        host = ops_vm.Host(output, control)
        work = Path(tempfile.mkdtemp(prefix="namespace-probe-vm-", dir=output))
        work_fd = os.open(work, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
        run_id = uuid.uuid4().hex
        report["runId"] = run_id
        with socket.socket() as selected:
            selected.bind(("127.0.0.1", 0))
            port = cast("tuple[str, int]", selected.getsockname())[1]
        state["checkpoint"] = "boot"
        vm.prepare_disk(host, work, pin)
        vm.create_seed(host, work, port, run_id)
        guest = vm.Guest(host, vm.qemu_command(work, port, run_id), qmp_path=work / "qmp.sock")
        host.guest = guest
        vm.boot(host, guest, work, port)
        # Existing cloud-init installs no fixture parent directory.
        _ = host.run(
            "probe-parent",
            vm.ssh_command(
                work,
                port,
                [
                    "sudo",
                    "-n",
                    "/usr/bin/mkdir",
                    "-m",
                    "700",
                    "/root/simplestchat-vm-fixture",
                ],
            ),
        )
        state["checkpoint"] = "stage"
        stage(host, work, port, sources)
        control.check()
        seconds = math.floor(control.end - time.monotonic() - 60)
        peers.require(MINIMUM_WORK <= seconds <= BODY_SECONDS)
        state["checkpoint"] = "guest"
        status, raw = host.run(
            "probe-eleven-cases",
            vm.ssh_command(
                work,
                port,
                [
                    "sudo",
                    "-n",
                    "/usr/bin/python3",
                    "-I",
                    "-B",
                    REMOTE + "/" + NAMES[0],
                    "run",
                    run_id,
                    str(seconds),
                ],
            ),
            timeout=seconds,
            accepted=OBSERVED_STATUSES,
        )
        value = observe_guest(status, raw, run_id, state)
        report["guestProbe"] = value
        peers.require(status == 0 and value.get("passed") is True and value.get("runId") == run_id)
        cases = value.get("cases")
        peers.require(isinstance(cases, list))
        rows = cast("list[dict[str, object]]", cases)
        peers.require(
            [row.get("name") for row in rows] == list(probe.CASES)
            and all(row.get("passed") is True for row in rows)
        )
        state["checkpoint"] = "inputs"
        peers.require(source_inputs() == sources)
        pins = {name: hashlib.sha256(raw).hexdigest() for name, raw in sources.items()}
        state["checkpoint"] = "final-source"
        _ = host.run(
            "probe-final-source-bindings",
            vm.ssh_command(
                work,
                port,
                [
                    "sudo",
                    "-n",
                    "/usr/bin/python3",
                    "-I",
                    "-B",
                    "-c",
                    VERIFY,
                    REMOTE,
                    json.dumps(pins, separators=(",", ":")),
                ],
            ),
        )
        report["passed"] = True
    except BaseException as error:  # noqa: BLE001 -- Always finish this exact disposable guest.
        first = error
        observe_failure(state, error, str(state["checkpoint"]))
        report["failure"] = "probe-unproven"
    finally:
        previous = first
        first = begin_finalization(control, first)
        if first is not None:
            observe_failure(
                state, first, "body-completion" if previous is None else str(state["checkpoint"])
            )
        cleanup: BaseException | None = None
        if guest is not None:
            cleanup = peers.settle((guest.close,))

            def observe_lifecycle() -> None:
                facts = ops_vm.lifecycle(guest, returned=cleanup is None)
                report["vmLifecycle"] = facts
                peers.require(ops_vm.positive_lifecycle(facts) and facts["qemuReturnCode"] == 0)

            cleanup = peers.settle((observe_lifecycle,), cleanup)
        if work is not None and work_fd is not None:

            def remove_owned_work() -> None:
                original, visible = os.fstat(work_fd), work.stat(follow_symlinks=False)
                peers.require(
                    (original.st_dev, original.st_ino) == (visible.st_dev, visible.st_ino)
                )
                shutil.rmtree(work)

            # Uncertain QEMU retirement retains its disk/keys privately; never remove live work.
            actions = [lambda: os.close(work_fd)]
            if cleanup is None:
                actions.insert(0, remove_owned_work)
            cleanup = peers.settle(actions, cleanup)
        report["cleanupPassed"] = cleanup is None
        state["cleanupPassed"] = cleanup is None
        state["cleanupException"] = exception_kind(cleanup)
        if cleanup is not None:
            observe_failure(state, cleanup, "cleanup")
        first = first or cleanup
        report["passed"] = (
            first is None
            and report["passed"] is True
            and control.status == 0
            and time.monotonic() < control.end + FINAL_SECONDS
        )
        if output.is_dir():
            state["checkpoint"] = "summary-output"
            write_private(
                output / "summary.json",
                json.dumps(report, indent=2, allow_nan=False).encode() + b"\n",
                mode=0o600,
            )
    return report


def begin_finalization(control: peers.Control, first: BaseException | None) -> BaseException | None:
    """Observe the original body before widening only known cleanup into its reserved tail."""
    first = peers.settle((control.check,), first)
    control.settling = True
    return first


def main(arguments: Sequence[str] | None = None) -> int:
    """Expose only an output destination; restore process state before the final fence."""
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    _ = parser.add_argument("--output", required=True, type=Path)
    options = parser.parse_args(arguments)
    output = cast("Path", options.output)
    control = peers.Control(BODY_SECONDS)
    restorations: list[Callable[[], None]] = []
    first: BaseException | None = None
    state: dict[str, object] = {"checkpoint": "admission"}
    report: dict[str, object] | None = None
    restored = False
    try:
        old_mask = os.umask(0o077)

        def restore_mask() -> None:
            _ = os.umask(old_mask)

        restorations.append(restore_mask)
        for number in (signal.SIGINT, signal.SIGTERM):
            previous = signal.getsignal(number)

            def restore(
                n: signal.Signals = number,
                handler: Callable[[int, FrameType | None], object] | int | None = previous,
            ) -> None:
                _ = signal.signal(n, handler)

            restorations.insert(0, restore)
            _ = signal.signal(number, control.signal)
        report = run(output, control, state)
    except BaseException as error:  # noqa: BLE001 -- Restore all state after every refusal.
        first = error
        observe_failure(state, error, str(state["checkpoint"]))
    finally:
        restoration = peers.settle(restorations)
        restored = restoration is None
        if restoration is not None:
            observe_failure(state, restoration, "restore")
        first = first or restoration
    try:
        peers.require(not control.status and time.monotonic() < control.end + FINAL_SECONDS)
        state["finalFencePassed"] = True
        peers.require(report is not None and report["passed"] is True and first is None)
    except BaseException as error:  # noqa: BLE001 -- Only bounded fixed metadata reaches output.
        first = first or error
        observe_failure(state, error, "final-fence")
        state["signalStatus"] = control.status
        metadata = failure_summary(state, report, first, restored=restored)
        original = state.get("_outputStamp")
        stamp = cast("tuple[int, ...]", original) if isinstance(original, tuple) else None
        _ = retain_failure(
            (output, stamp, control.end + FINAL_SECONDS), metadata, first_error(state, first)
        )
        return control.status or 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
