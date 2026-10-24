"""Five future owned-Debian exec probes; this module is not wired into the keeper.

Integration must explicitly project this module and vps_ops_bootstrap.py, copy
them plus the existing guest reference into the separate closed vps-source tree,
retain the existing 28 installer cases, and extend the typed receipt separately.
Call exercise after original transfer reception and before the installer cases;
the existing guard deliberately refuses an already installed candidate. The
stage integration creates /srv/simplestchat-bench/artifacts root-owned0700.
The parent receives the existing original Options/Receiver/Control. It never
acquires an artifact or runs a workload. Each child uses the real bootstrap
invoke path. A fixture decorator interposes only at its final exec boundary and
still delegates the actual same-FD kernel exec (except the deliberate wrong-image
negative). All stage bytes come from the original authenticated receiver.

OS01 observes the mapped ELF and absence of open child FDs to its dev/inode while
E is withheld, before Rust's candidate/proc-self-exe opens. Rust can already own
root/input/signal peers; FD numbers can be reused. Its final framing1 is separate
from successful kernel exec. No inherited original-FD or checked CLOEXEC-close
claim crosses exec. The trusted Linux procfs assumption is explicit.
OS05 delays the final boundary until the actual original invoke N+61s, without
changing N, then requires Rust's deadline124. No capacity policy/action runs.
The extension needs at least 125s remaining in the original guest budget.
"""

# ruff: noqa: E402 -- Validate each exact guest import namespace before adding it to sys.path.

from __future__ import annotations

import fcntl
import hashlib
import io
import json
import os
import select
import stat
import subprocess
import sys
import tarfile
import time
from contextlib import suppress
from pathlib import Path
from typing import TYPE_CHECKING, cast
from unittest.mock import patch

OWNER_UID = 0
OWNER_GID = 0
IMPORT_NAMES = 64


def import_directory(
    path: Path, files: frozenset[str], directories: frozenset[str], mode: int
) -> None:
    """Refuse ignored/source/native siblings before making a fixed tree importable."""
    parent = path.lstat()
    if (
        not stat.S_ISDIR(parent.st_mode)
        or parent.st_uid != OWNER_UID
        or parent.st_gid != OWNER_GID
        or parent.st_mode & 0o022
    ):
        message = "vps_probe_import_namespace"
        raise ValueError(message)
    names: set[str] = set()
    with os.scandir(path) as entries:
        for entry in entries:
            if len(names) >= IMPORT_NAMES or entry.name not in files | directories:
                message = "vps_probe_import_namespace"
                raise ValueError(message)
            names.add(entry.name)
            value = entry.stat(follow_symlinks=False)
            wanted = 0o700 if entry.name in directories else mode
            ordinary = (
                stat.S_ISDIR(value.st_mode)
                if entry.name in directories
                else stat.S_ISREG(value.st_mode) and value.st_nlink == 1
            )
            if (
                not ordinary
                or value.st_uid != OWNER_UID
                or value.st_gid != OWNER_GID
                or stat.S_IMODE(value.st_mode) != wanted
            ):
                message = "vps_probe_import_namespace"
                raise ValueError(message)
    if names != set(files | directories):
        message = "vps_probe_import_namespace"
        raise ValueError(message)


def import_failure() -> NoReturn:
    """Keep pre-import refusal fixed and free of Python tracebacks/private names."""
    _ = sys.stderr.write("VPS exec probe imports are unproven.\n")
    raise SystemExit(1)


# -I omits the script directory. Check the exact staged namespace BEFORE adding
# it: -B/fresh pycache do not exclude ignored legacy pyc/native/package siblings.
if __name__ == "__main__":
    try:
        import_directory(
            Path("/root/simplestchat-vm-fixture/vps-source"),
            frozenset(
                {
                    "security_vm_ops_guest.py",
                    "security_vm_vps_guest.py",
                    "vps_ops_bootstrap.py",
                }
            ),
            frozenset(),
            0o600,
        )
    except (OSError, ValueError):
        import_failure()
    sys.path.insert(0, "/root/simplestchat-vm-fixture/vps-source")

import security_vm_ops_guest as guest

if __name__ == "__main__":
    try:
        import_directory(guest.LIBRARY, guest.BASE | guest.FETCH | guest.FORCE, frozenset(), 0o644)
    except (OSError, ValueError):
        import_failure()
    sys.path.insert(1, str(guest.LIBRARY))

import bounded_process
import vps_ops_bootstrap as bootstrap

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping, Sequence
    from typing import NoReturn

CASES = (
    ("vps-fd-cloexec", 1, b"ops_capacity_framing\n"),
    ("vps-stage-exchange", 1, b"ops_capacity_identity\n"),
    ("vps-stage-unlinked", 1, b"ops_capacity_identity\n"),
    ("vps-wrong-image", 1, b"ops_capacity_identity\n"),
    ("vps-origin-expired", 124, b"ops_capacity_deadline\n"),
)
LOOKUP = b'{"schemaVersion":1,"scope":"OpsCapacityOperation","action":"lookup"}'
PROBES = guest.ROOT / "vps-probes"
SCRIPT = guest.ROOT / "vps-source" / "security_vm_vps_guest.py"
MAX_TRACE = 8192
MAX_FDS = 128
STAMP_FIELDS = 9
CHILD_ARGUMENTS = 3
GATE_MIN_FD = 3
RUN_HEX_BYTES = 32
CASE_SECONDS = 120
REQUIRED_SECONDS = 125
EXPIRED_NS = 61 * 1_000_000_000
READ_FLAGS = os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC


def check_case(name: str) -> None:
    """No executable, host, root or command selector is accepted."""
    guest.require(any(case[0] == name for case in CASES))


def pin(data: bytes) -> dict[str, object]:
    """Measure fixture framing bytes independently of artifact member pins."""
    return {"sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)}


def header(options: guest.Options, name: str) -> bytes:
    """Keep original CI/artifact/transfer/member declarations and a distinct run."""
    check_case(name)
    run = hashlib.sha256((options.run_id + ":" + name).encode()).hexdigest()[:32]
    value = {
        "schemaVersion": 1,
        "scope": "OpsCapacityBootstrap",
        "mode": "invoke",
        "run": run,
        "selection": options.selected,
        "artifactId": options.artifact_id,
        "transfer": {"sha256": options.transfer_sha256, "bytes": options.transfer_bytes},
        "members": {
            member: {"sha256": options.pins[member][0], "bytes": options.pins[member][1]}
            for member in guest.NAMES
        },
        "request": pin(LOOKUP),
    }
    encoded = json.dumps(value, separators=(",", ":")).encode() + b"\n"
    _ = bootstrap.Header.parse(encoded)
    return encoded


def stage(
    options: guest.Options, receiver: guest.Receiver, raw: bytes, control: guest.Control
) -> None:
    """Copy only original measured files; close every writer before child exec."""
    value = bootstrap.Header.parse(raw)
    owners = bootstrap.Owners()
    copied: guest.Receiver | None = None
    primary: BaseException | None = None
    try:
        directories = bootstrap.open_parent(owners)
        os.mkdir("ops-runtime." + value.run, 0o700, dir_fd=directories[-1].descriptor.fd)
        copied = guest.Receiver(bootstrap.ROOT / ("ops-runtime." + value.run))
        for member in guest.NAMES:
            held = receiver.files[member]
            held.check(receiver.parent, control.check)
            _ = held.file.seek(0)
            item = tarfile.TarInfo(member)
            item.size = options.pins[member][1]
            copied.member(item, held.file, options.pins[member], control.check)
            _ = held.file.seek(0)
        os.fsync(copied.parent)
        os.fsync(directories[-1].descriptor.fd)
    except BaseException as error:  # noqa: BLE001 -- Preserve primary through all fixture peers.
        primary = error
    finally:
        actions = [copied.close] if copied is not None else []
        primary = guest.settle((*actions, lambda: close_owners(owners)), primary)
    if primary is not None:
        raise primary


def fault(
    name: str,
    selected: bootstrap.Header,
    owners: bootstrap.Owners,
    fd: int,
    control: bootstrap.Control,
) -> int:
    """Select only the three deliberate custody faults after original admission."""
    canonical = bootstrap.ROOT / ("ops-runtime." + selected.run) / guest.NAMES[-1]
    if name == "vps-stage-exchange":
        replacement = PROBES / name / "replacement"
        extra = owners.add(alternate(fd, replacement, selected.members[-1], control))
        extra.close()
        _ = replacement.replace(canonical)
    elif name == "vps-stage-unlinked":
        # Keep canonical inputs valid. Skipping mapped-image custody would
        # reach missing lookup evidence (IO), not this identity refusal.
        unlinked = PROBES / name / "unlinked-image"
        execution = owners.add(alternate(fd, unlinked, selected.members[-1], control)).fd
        unlinked.unlink()
        return execution
    elif name == "vps-wrong-image":
        return owners.add(
            alternate(fd, PROBES / name / "other-image", selected.members[-1], control)
        ).fd
    return fd


def close_owners(owners: bootstrap.Owners) -> None:
    """Adapt the receiver's existing all-attempt close result without losing it."""
    error = owners.close()
    if error is not None:
        raise error


def alternate(
    fd: int, destination: Path, selected: bootstrap.Pin, control: bootstrap.Control
) -> int:
    """Create a different inode with the exact signed bytes, never another program."""
    writer = bootstrap.Descriptor(
        os.open(
            destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600
        )
    )
    primary: BaseException | None = None
    written: os.stat_result | None = None
    try:
        guest.require(os.lseek(fd, 0, os.SEEK_SET) == 0)
        remaining = selected.count
        while remaining:
            control.check()
            data = os.read(fd, min(65536, remaining))
            guest.require(data)
            bootstrap.write_all(writer.fd, data, control)
            remaining -= len(data)
        guest.require(os.read(fd, 1) == b"")
        os.fsync(writer.fd)
        written = os.fstat(writer.fd)
    except BaseException as error:  # noqa: BLE001 -- One writer close still runs after copy failure.
        primary = error
        control.remember(error)
    finally:
        primary = guest.settle((writer.close,), primary)
    if primary is not None:
        raise primary
    if written is None:
        bootstrap.fail("unproven")
    reader = os.open(destination, READ_FLAGS)
    try:
        guest.require(guest.stamp(os.fstat(reader)) == guest.stamp(written))
        bootstrap.regular(os.fstat(reader), 0o600, selected.count)
        bootstrap.hash_fd(reader, selected, control)
        guest.require(guest.stamp(os.fstat(reader)) == guest.stamp(written))
        # The checked writer close precedes executable mode on this reader.
        os.fchmod(reader, 0o755)
        guest.require(os.lseek(fd, 0, os.SEEK_SET) == 0)
    except BaseException as error:
        control.remember(error)
        _ = guest.settle((lambda: os.close(reader),), error)
        raise
    return reader


def gate(fd: int, control: bootstrap.Control) -> None:
    """Wait for the trusted parent's one LF+EOF while preserving the original clock."""
    guest.require(stat.S_ISFIFO(os.fstat(fd).st_mode))
    for wanted in (b"\n", b""):
        while True:
            control.check()
            if select.select([fd], [], [], 0.05)[0]:
                guest.require(os.read(fd, 1) == wanted)
                break


def trace(name: str, value: dict[str, object], control: bootstrap.Control) -> None:
    """Seal a bounded pre-exec observation before asking the parent to release it."""
    data = json.dumps(value, separators=(",", ":")).encode() + b"\n"
    guest.require(len(data) <= MAX_TRACE)
    descriptor = bootstrap.Descriptor(
        os.open(
            PROBES / name / "preexec.json",
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
            0o600,
        )
    )
    primary: BaseException | None = None
    try:
        bootstrap.write_all(descriptor.fd, data, control)
        os.fsync(descriptor.fd)
    except BaseException as error:  # noqa: BLE001 -- Preserve write failure through the close.
        primary = error
        control.remember(error)
    finally:
        primary = guest.settle((descriptor.close,), primary)
    if primary is not None:
        raise primary
    # This directory is only synchronization: the parent still validates the
    # complete trace before release and independently checks Linux procfs.
    (PROBES / name / "preexec.ready").mkdir(mode=0o700)


def child_guard() -> None:
    """Require Linux and root for the private child route inside the marked guest."""
    guest.require(os.geteuid() == 0 and sys.platform == "linux")
    marker, _ = guest.read_fixed(guest.MARKER, 64, 0o600)
    guest.require(len(marker) == RUN_HEX_BYTES + 1 and marker[-1:] == b"\n")
    _ = bootstrap.hexadecimal(marker[:-1].decode("ascii"), RUN_HEX_BYTES)


def child(name: str, inherited_gate: int) -> int:
    """Interpose fixed faults after real bootstrap admission; delegate real FD exec."""
    released = bootstrap.Descriptor(inherited_gate)
    original_run, original_exec = bootstrap.run, os.execve

    def observed_run(
        selected: bootstrap.Header, control: bootstrap.Control, owners: bootstrap.Owners
    ) -> None:
        def execute(fd: int, argv: Sequence[str], env: Mapping[str, str]) -> NoReturn:
            guest.require(type(fd) is int and not os.get_inheritable(fd))
            guest.require(fcntl.fcntl(fd, fcntl.F_GETFL) & os.O_ACCMODE == os.O_RDONLY)
            guest.require(os.lseek(fd, 0, os.SEEK_CUR) == 0)
            execution = fault(name, selected, owners, fd, control)
            guest.require(not os.get_inheritable(execution))
            guest.require(fcntl.fcntl(execution, fcntl.F_GETFL) & os.O_ACCMODE == os.O_RDONLY)
            bootstrap.hash_fd(execution, selected.members[-1], control)
            value: dict[str, object] = {
                "schemaVersion": 1,
                "case": name,
                "originMonotonicNs": control.origin,
                "headerSha256": hashlib.sha256(selected.raw.encode()).hexdigest(),
                "execution": list(guest.stamp(os.fstat(execution))),
                "cloexec": True,
                "readonly": True,
            }
            trace(name, value, control)
            gate(released.fd, control)
            released.close()
            if name == "vps-origin-expired":
                # Simulate time charged between the last Python fence and kernel
                # exec. Keep N untouched and preserve signals during this delay.
                while time.monotonic_ns() < control.origin + EXPIRED_NS:
                    if control.failure is not None:
                        raise control.failure
                    time.sleep(0.01)
            # Exact observed readonly FD; no executable path or PATH fallback.
            original_exec(execution, list(argv), env)

        with patch.object(os, "execve", execute):
            original_run(selected, control, owners)

    primary: BaseException | None = None
    result = 1
    returned = False
    try:
        check_case(name)
        child_guard()
        os.set_inheritable(inherited_gate, False)  # noqa: FBT003 -- Stdlib positional-only flag.
        with patch.object(bootstrap, "run", observed_run):
            result = bootstrap.main([])
            returned = True
    except BaseException as error:  # noqa: BLE001 -- Finalize inherited gate even on construction errors.
        primary = error
    finally:
        closed = guest.settle((released.close,))
        if closed is not None and returned and result != 0:
            # Preserve the original bootstrap status, but an expected negative
            # probe must not hide this independent fixture close failure.
            with suppress(OSError):
                _ = os.write(2, b"vps_exec_probe_close\n")
        elif primary is None:
            primary = closed
    if primary is not None:
        raise primary
    return result


def read_trace(name: str, raw: bytes) -> tuple[int, tuple[int, ...]]:
    """Validate only fixed fields, never accepting candidate JSON as authority."""
    data, _ = guest.read_fixed(PROBES / name / "preexec.json", MAX_TRACE, 0o600)
    return parse_trace(data, name, raw)


def parse_trace(data: bytes, name: str, raw: bytes) -> tuple[int, tuple[int, ...]]:
    """Decode the fixed trusted pre-exec projection separately from file custody."""
    guest.require(len(data) <= MAX_TRACE)
    value = guest.exact(
        guest.object_json(data),
        {
            "schemaVersion",
            "case",
            "originMonotonicNs",
            "headerSha256",
            "execution",
            "cloexec",
            "readonly",
        },
    )
    origin = value["originMonotonicNs"]
    record = value["execution"]
    guest.require(type(value["schemaVersion"]) is int and value["schemaVersion"] == 1)
    guest.require(value["case"] == name and value["cloexec"] is True and value["readonly"] is True)
    guest.require(value["headerSha256"] == hashlib.sha256(raw[:-1]).hexdigest())
    guest.require(type(origin) is int and 0 < origin <= time.monotonic_ns())
    guest.require(isinstance(record, list))
    items = cast("list[object]", record)
    guest.require(len(items) == STAMP_FIELDS and all(type(v) is int and v >= 0 for v in items))
    return cast("int", origin), tuple(cast("list[int]", record))


def candidate_fds_absent(pid: int, image: tuple[int, ...]) -> bool:
    """Inspect actual child FD targets, allowing descriptor-number reuse."""
    directory = Path("/proc") / str(pid) / "fd"
    with os.scandir(directory) as entries:
        count = 0
        for entry in entries:
            count += 1
            guest.require(count <= MAX_FDS and entry.name.isascii() and entry.name.isdecimal())
            try:
                target = entry.stat(follow_symlinks=True)
            except FileNotFoundError:
                continue
            if (target.st_dev, target.st_ino) == image[:2]:
                return False
    return True


def send_header(
    child_process: subprocess.Popen[bytes], data: bytes, checkpoint: Callable[[], None]
) -> None:
    """Write only header bytes, leaving E and EOF under the parent's control."""
    guest.require(child_process.stdin is not None)
    stream = cast("io.BufferedWriter", child_process.stdin)
    os.set_blocking(stream.fileno(), False)
    pending = memoryview(data)
    while pending:
        checkpoint()
        if select.select([], [stream], [], 0.05)[1]:
            try:
                count = os.write(stream.fileno(), pending)
            except BlockingIOError:
                continue
            guest.require(count > 0)
            pending = pending[count:]


def observe_exec(
    process: subprocess.Popen[bytes],
    name: str,
    raw: bytes,
    write_gate: bootstrap.Descriptor,
    check: Callable[[], None],
) -> tuple[int, bool]:
    """Read sealed trusted pre-exec facts, then observe the real kernel image/FDs."""
    while not (PROBES / name / "preexec.ready").is_dir():
        check()
        guest.require(not bounded_process.leader_exited(process.pid))
        time.sleep(0.01)
    origin, image = read_trace(name, raw)
    guest.require(os.write(write_gate.fd, b"\n") == 1)
    write_gate.close()
    while not bounded_process.leader_exited(process.pid):
        check()
        try:
            mapped = (Path("/proc") / str(process.pid) / "exe").stat()
        except FileNotFoundError:
            continue
        if (mapped.st_dev, mapped.st_ino) == image[:2]:
            guest.require(guest.stamp(mapped) == image)
            guest.require(candidate_fds_absent(process.pid, image))
            return origin, True
        time.sleep(0.01)
    return origin, False


def run_case(name: str, raw: bytes, control: guest.Control) -> dict[str, object]:
    """Own one bounded trusted child and all pipes through its negative result."""
    check_case(name)
    control.check()
    end = min(control.end, time.monotonic() + CASE_SECONDS)

    def check() -> None:
        control.check()
        if time.monotonic() >= end:
            control.status = control.status or guest.DEADLINE_STATUS
            message = "ops_guest_deadline"
            raise guest.OpsError(message)

    def healthy() -> bool:
        try:
            check()
        except (OSError, ValueError):
            return False
        return True

    reader, writer = os.pipe()
    read_gate, write_gate = bootstrap.Descriptor(reader), bootstrap.Descriptor(writer)
    process: subprocess.Popen[bytes] | None = None
    output, error = io.BytesIO(), io.BytesIO()
    primary: BaseException | None = None
    status, origin, observed = 1, 0, False
    try:
        check()
        guest.empty_cache()
        check()
        process = subprocess.Popen(  # noqa: S603 -- Fixed trusted fixture, one closed case and owned pipe FD.
            [
                "/usr/bin/python3",
                "-I",
                "-B",
                "-X",
                "pycache_prefix=" + str(guest.ROOT / "python-cache"),
                str(SCRIPT),
                "--child",
                name,
                str(reader),
            ],
            pass_fds=(reader,),
            close_fds=True,
            cwd=guest.ROOT,
            env=bootstrap.ENVIRONMENT,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
        read_gate.close()
        send_header(process, raw, check)
        origin, observed = observe_exec(process, name, raw, write_gate, check)
        remaining = end - time.monotonic()
        check()
        status = bounded_process.pump(
            process,
            output,
            error,
            bounded_process.Limits(timeout=remaining, stdout=65536, stderr=16384, healthy=healthy),
            input_data=b"" if name in (CASES[0][0], CASES[-1][0]) else LOOKUP,
        )
    except BaseException as original:  # noqa: BLE001 -- All acquired peers settle after every fault.
        primary = original
        control.status = control.status or 1
        if process is not None:
            primary = guest.settle((lambda: bounded_process.stop(process),), primary)
    finally:
        actions = [read_gate.close, write_gate.close]
        if process is not None:
            actions.extend(
                stream.close
                for stream in (process.stdin, process.stdout, process.stderr)
                if stream is not None
            )
        primary = guest.settle(actions, primary)
    if primary is not None:
        raise primary
    expected = next(case for case in CASES if case[0] == name)
    guest.require(
        status == expected[1] and output.getvalue() == b"" and error.getvalue() == expected[2]
    )
    guest.require(observed or name == CASES[-1][0])
    if name == CASES[-1][0]:
        guest.require(time.monotonic_ns() >= origin + EXPIRED_NS)
    control.check()
    return {
        "name": name,
        "exitStatus": status,
        "passed": True,
        "kernelImageObserved": observed,
        "originalCloexec": True,
        "originalReadonly": True,
    }


def exercise(
    options: guest.Options, receiver: guest.Receiver, control: guest.Control
) -> list[dict[str, object]]:
    """Future explicit integration seam; no old case/receipt/source list changes."""
    guest.guard(options)
    control.check()
    guest.require(control.end - time.monotonic() >= REQUIRED_SECONDS)
    PROBES.mkdir(mode=0o700)
    records: list[dict[str, object]] = []
    for name, _, _ in CASES:
        control.check()
        (PROBES / name).mkdir(mode=0o700)
        raw = header(options, name)
        stage(options, receiver, raw, control)
        records.append(run_case(name, raw, control))
        receiver.check()
        for held in receiver.files.values():
            held.check(receiver.parent, control.check)
    guest.empty_cache()
    control.check()
    return records


def main(arguments: Sequence[str]) -> int:
    """Only the fixed child route exists; source integration remains explicit."""
    if (
        len(arguments) != CHILD_ARGUMENTS
        or arguments[0] != "--child"
        or not arguments[2].isascii()
        or not arguments[2].isdecimal()
    ):
        return 2
    try:
        inherited = int(arguments[2])
        guest.require(GATE_MIN_FD <= inherited <= MAX_FDS)
        guest.require(Path(__file__).absolute() == SCRIPT)
        return child(arguments[1], inherited)
    except (OSError, ValueError, RecursionError):
        _ = sys.stderr.write("VPS exec probe is unproven.\n")
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
