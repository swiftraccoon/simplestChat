"""Own one authenticated-artifact Ops check in a disposable, marked Debian VM.

The archive and its member identities are original controller-memory pins. This
keeper executes no candidate on the controller, accepts no host/image/command
selector, and never treats candidate JSON as authentication. Only its closed
metadata receipt may be published; command streams and keys remain private.
The Rust owner launches this keeper from its fresh, closed source projection.
Derived ROOT paths and Ansible inputs refer to that projection, not the caller checkout.
"""

from __future__ import annotations

import fcntl
import hashlib
import json
import math
import os
import re
import shlex
import shutil
import signal
import socket
import stat
import sys
import tarfile
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, cast, final, override

# -I excludes the caller checkout. Restore only the Rust owner's closed,
# projected BUILD; security_context derives ROOT from that same projection.
sys.path.insert(0, str(Path(__file__).resolve().parent))
import security_vm as vm
import security_vm_ops_guest as protocol
from security_context import ROOT, executable
from security_tools import ToolError, write_private

# isort: split
# security_context installs the one original measured operations-helper directory.
import bounded_process

if TYPE_CHECKING:
    from collections.abc import Mapping, Sequence
    from typing import BinaryIO

SECONDS = 1800
MAX_RECEIPT = 65536
MAX_RETURN_CODE = 255
FAILURES = frozenset(
    {
        "guard",
        "transfer",
        "boot",
        "stage",
        "cases",
        "cleanup",
        "cancelled",
        "deadline",
        "unavailable",
    }
)


@final
class Input:
    """Borrow the original regular stdin inode, retaining a distinct explicit-close owner."""

    def __init__(self, descriptor: int, options: protocol.Options) -> None:
        """No reopen, pipe adapter or writer is accepted at this controller boundary."""
        self.file = os.fdopen(os.dup(descriptor), "rb")
        self.options = options
        try:
            self.original = os.fstat(self.file.fileno())
            protocol.require(
                stat.S_ISREG(self.original.st_mode)
                and stat.S_IMODE(self.original.st_mode) == protocol.PRIVATE_MODE
                and self.original.st_uid == os.getuid()
                and self.original.st_gid == os.getgid()
                and self.original.st_nlink == 1
                and self.original.st_size == options.transfer_bytes
                and fcntl.fcntl(self.file.fileno(), fcntl.F_GETFL) & os.O_ACCMODE == os.O_RDONLY
                and self.file.tell() == 0
            )
        except BaseException:
            self.file.close()
            raise

    def check(self, control: protocol.Control) -> None:
        """Compare the same original full stamp and whole digest; reset its cursor explicitly."""
        control.check()
        protocol.require(
            protocol.stamp(os.fstat(self.file.fileno())) == protocol.stamp(self.original)
        )
        _ = self.file.seek(0)
        protocol.require(
            protocol.digest_stream(self.file, self.options.transfer_bytes, control.check)
            == self.options.transfer_sha256
        )
        protocol.require(
            protocol.stamp(os.fstat(self.file.fileno())) == protocol.stamp(self.original)
        )
        _ = self.file.seek(0)

    def admit(self, control: protocol.Control) -> None:
        """Authenticate before parsing; use mature USTAR reading without extracting anything."""
        self.check(control)
        observed: list[str] = []
        end = 0
        with tarfile.open(fileobj=self.file, mode="r:") as archive:
            for item in archive:
                control.check()
                protocol.require(
                    len(observed) < len(protocol.NAMES)
                    and item.name == protocol.NAMES[len(observed)]
                    and item.type == tarfile.REGTYPE
                    and not item.pax_headers
                    and not item.linkname
                    and item.uid == 0
                    and item.gid == 0
                    and item.mode == protocol.PRIVATE_MODE
                    and item.offset == end
                    and item.offset_data == item.offset + 512
                )
                expected_sha, count = self.options.pins[item.name]
                protocol.require(item.size == count)
                cursor = self.file.tell()
                _ = self.file.seek(item.offset + 257)
                protocol.require(self.file.read(6) == b"ustar\0")
                _ = self.file.seek(cursor)
                member = archive.extractfile(item)
                protocol.require(member is not None)
                with cast("BinaryIO", member) as source:
                    if item.name == protocol.NAMES[1]:
                        data = source.read(protocol.MAX_SELECTION + 1)
                        protocol.require(
                            data == self.options.selection_raw
                            and hashlib.sha256(data).hexdigest() == expected_sha
                        )
                    else:
                        protocol.require(
                            protocol.digest_stream(source, count, control.check) == expected_sha
                        )
                observed.append(item.name)
                end = item.offset_data + ((item.size + 511) // 512) * 512
        protocol.require(tuple(observed) == protocol.NAMES)
        _ = self.file.seek(end)
        tail = self.file.read(10241)
        protocol.require(
            protocol.TAR_END <= len(tail) <= protocol.TAR_PADDING
            and not any(tail)
            and self.file.read(1) == b""
        )
        self.check(control)

    def close(self) -> None:
        """Settle the duplicated parent owner; the original borrowed stdin is not consumed."""
        self.file.close()


class Host(vm.Host):
    """Keep existing mature VM machinery, with one original deadline and borrowed archive input."""

    def __init__(self, output: Path, control: protocol.Control) -> None:
        """Capture the original entry deadline and credential-free legacy VM environment."""
        self.control: protocol.Control = control
        self.guest: vm.Guest | None = None
        super().__init__(output, control.end)

    def healthy(self) -> bool:
        """Check cancellation/deadline and the same original QEMU while command pipes are idle."""
        try:
            self.control.check()
            if self.guest is not None:
                self.guest.require_alive()
        except (OSError, ValueError, ToolError):
            return False
        else:
            return True

    @override
    def run(
        self,
        name: str,
        argv: Sequence[str],
        *,
        timeout: float = 120,
        accepted: tuple[int, ...] = (0,),
        input_file: BinaryIO | None = None,
    ) -> tuple[int, bytes]:
        """Resample before spawn; each raw stream is bounded and private, never a public receipt."""
        protocol.require(
            re.fullmatch(r"[a-z0-9-]{1,64}", name)
            and len(self.checks) < vm.MAX_COMMANDS
            and math.isfinite(timeout)
            and timeout > 0
        )
        self.control.check()
        prefix = self.output / f"{len(self.checks):02d}-{name}"
        files: list[BinaryIO] = []
        primary: BaseException | None = None
        status, data = 1, b""
        try:
            for extension in (".stdout", ".stderr"):
                descriptor = os.open(
                    prefix.with_suffix(extension),
                    os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
                    0o600,
                )
                stream = os.fdopen(descriptor, "w+b")
                files.append(stream)
                os.fchmod(stream.fileno(), 0o600)
            self.control.check()
            remaining = min(timeout, self.deadline - time.monotonic())
            if remaining <= 0:
                self.control.status = self.control.status or protocol.DEADLINE_STATUS
            protocol.require(remaining > 0, "ops_guest_deadline")
            status, _, _ = bounded_process.run(
                argv,
                cwd=ROOT,
                env=self.env,
                input_file=input_file,
                output=files[0],
                error=files[1],
                limits=bounded_process.Limits(
                    timeout=remaining,
                    stdout=vm.MAX_STREAM,
                    stderr=vm.MAX_STREAM,
                    healthy=self.healthy,
                ),
            )
            for stream in files:
                stream.flush()
            _ = files[0].seek(0)
            data = files[0].read(vm.MAX_STREAM + 1)
            protocol.require(len(data) <= vm.MAX_STREAM)
            self.control.check()
        except BaseException as error:  # noqa: BLE001 -- Retain the failure through every owned close.
            primary = error
        finally:
            primary = protocol.settle(tuple(stream.close for stream in files), primary)
        self.checks.append({"name": name, "exitStatus": status})
        if primary is not None:
            raise primary
        protocol.require(status in accepted)
        return status, data


def inventory(work: Path, port: int, options: protocol.Options) -> Path:
    """No app/provider variables: one generated loopback host and the original fixture identity."""
    value = {
        "benchmark_hosts": {
            "hosts": {
                "vmfixture": {
                    "ansible_host": "127.0.0.1",
                    "ansible_port": port,
                    "ansible_user": "fixture",
                    "ansible_python_interpreter": "/usr/bin/python3",
                    "ansible_ssh_private_key_file": str(work / "client"),
                    "ansible_ssh_args": shlex.join(vm.ssh_options(work)),
                    "ansible_become": True,
                    "vm_fixture_run_id": options.run_id,
                    "vm_fixture_revision": options.selected["revision"],
                }
            }
        }
    }
    path = work / "inventory.json"
    write_private(path, json.dumps(value).encode(), mode=0o600)
    return path


def stage(host: Host, guest: vm.Guest, work: Path, port: int, options: protocol.Options) -> None:
    """Run only the fixed trusted assertion playbook; no old application stage is invoked."""
    guest.require_alive()
    path = inventory(work, port, options)
    _, output = host.run(
        "ops-stage",
        [executable("ansible-playbook"), "-i", str(path), str(ROOT / "security/vm/ops-stage.yml")],
        timeout=180,
    )
    matches = list(vm.RECAP.finditer(output.decode("utf-8")))
    protocol.require(len(matches) == 1 and int(matches[0][3]) == 0 and int(matches[0][4]) == 0)
    guest.require_alive()


def guest_observation(data: bytes, options: protocol.Options, status: int) -> dict[str, object]:
    """Validate a strict ordered prefix without publishing arbitrary guest output."""
    value = protocol.exact(
        protocol.object_json(data),
        {"schemaVersion", "scope", "runId", "cases", "install", "passed", "failure"},
    )
    protocol.require(
        type(value["schemaVersion"]) is int
        and value["schemaVersion"] == 1
        and value["scope"] == "OpsDebianGuestReceipt"
        and value["runId"] == options.run_id
        and type(value["passed"]) is bool
        and isinstance(value["cases"], list)
    )
    rows = cast("list[object]", value["cases"])
    protocol.require(len(rows) <= len(protocol.CASES))
    for item, (name, expected) in zip(rows, protocol.CASES, strict=False):
        row = protocol.exact(item, {"name", "exitStatus", "passed"})
        protocol.require(row["name"] == name and type(row["passed"]) is bool)
        _ = protocol.integer(row["exitStatus"], 255, minimum=0)
        if row["passed"] is True:
            protocol.require(row["exitStatus"] == expected)
    install = protocol.exact(value["install"], {"firstChanged", "secondChanged"})
    protocol.require(all(item is None or type(item) is bool for item in install.values()))
    if value["passed"] is True:
        protocol.require(
            status == 0
            and len(rows) == len(protocol.CASES)
            and all(cast("dict[str, object]", row)["passed"] is True for row in rows)
            and install == {"firstChanged": True, "secondChanged": False}
            and value["failure"] is None
        )
    else:
        protocol.require(
            status != 0 and isinstance(value["failure"], str) and value["failure"] in FAILURES
        )
    return value


def exercise(
    host: Host, guest: vm.Guest, work: Path, port: int, archive: Input
) -> dict[str, object]:
    """Use the same retained stdin file with one fixed SSH command."""
    options = archive.options
    archive.check(host.control)
    guest.require_alive()
    command = [
        "sudo",
        "-n",
        "/usr/bin/python3",
        "-I",
        "-B",
        "-X",
        "pycache_prefix=/root/simplestchat-vm-fixture/python-cache",
        "/root/simplestchat-vm-fixture/security_vm_ops_guest.py",
        *options.argv(),
    ]
    status, output = host.run(
        "ops-cases",
        vm.ssh_command(work, port, command),
        timeout=SECONDS,
        accepted=(0, 1, 2, 124, 130, 143),
        input_file=archive.file,
    )
    archive.check(host.control)
    guest.require_alive()
    return guest_observation(output, options, status)


def lifecycle(guest: vm.Guest, *, returned: bool) -> dict[str, object]:
    """Only actual owned QEMU, drainer, authenticated SSH and QMP facts enter this DTO."""
    raw = guest.diagnostics()
    qmp = cast("dict[str, object]", raw.get("qmp", {}))
    cloud = cast("dict[str, object]", raw.get("cloudInit", {}))
    status = raw.get("cleanupReturnCode")
    protocol.require(
        status is None or (type(status) is int and -MAX_RETURN_CODE <= status <= MAX_RETURN_CODE)
    )
    return {
        "qemuReaped": type(status) is int,
        "qemuReturnCode": status,
        "authenticatedSsh": raw.get("authenticatedSsh") is True,
        "drainerCompleted": raw.get("drainer") == "completed",
        "qmpConnected": qmp.get("connected") is True,
        "qmpGreetingReceived": qmp.get("greetingReceived") is True,
        "qmpCapabilitiesNegotiated": qmp.get("capabilitiesNegotiated") is True,
        "qmpEofObserved": qmp.get("eofObserved") is True,
        "qmpPassed": qmp.get("failure") is None and bool(qmp),
        "cloudInitPassed": cloud.get("passed") is True,
        "closeReturned": returned,
    }


def positive_lifecycle(value: Mapping[str, object]) -> bool:
    """Require stream/QMP EOF facts as well as a close return."""
    return (
        all(item is True for key, item in value.items() if key != "qemuReturnCode")
        and type(value["qemuReturnCode"]) is int
    )


def receipt(options: protocol.Options, pin: vm.CloudImage) -> dict[str, object]:
    """Allocate the exact closed public projection before any fallible VM work."""
    value: dict[str, object] = {
        "schemaVersion": 1,
        "scope": "OpsDebianKeeperReceipt",
        "runId": options.run_id,
        "selection": options.selected,
        "artifactId": options.artifact_id,
        "transferSha256": options.transfer_sha256,
        "transferBytes": options.transfer_bytes,
        "installRequestSha256": options.install_sha256,
        "cloudImageSha512": pin.sha512,
        "install": {"firstChanged": None, "secondChanged": None},
        "cases": [],
        "guestExecuted": False,
        "runtimeValidated": False,
        "installValidated": False,
        "cleanupPassed": False,
        "passed": False,
        "failure": "guard",
        "vmLifecycle": None,
    }
    for name, prefix in zip(protocol.NAMES, ("manifest", "selection", "binary"), strict=True):
        value[prefix + "Sha256"], value[prefix + "Bytes"] = options.pins[name]
    return value


@dataclass
class Owners:
    """Retain every acquired peer until exact VM closure and original-input checks."""

    archive: Input | None = None
    guest: vm.Guest | None = None
    work: Path | None = None

    def finish(self, control: protocol.Control, report: dict[str, object]) -> BaseException | None:
        """Attempt every peer, keeping uncertain cleanup negative and the first error intact."""
        primary: BaseException | None = None
        if self.guest is not None:
            guest = self.guest
            failure = protocol.settle((guest.close,))
            primary = failure

            def observe() -> None:
                facts = lifecycle(guest, returned=failure is None)
                report["vmLifecycle"] = facts
                protocol.require(positive_lifecycle(facts) and guest.failure is None)

            primary = protocol.settle((observe,), primary)
        if self.archive is not None:
            archive = self.archive
            primary = protocol.settle((lambda: archive.check(control), archive.close), primary)
        if self.work is not None:
            work = self.work
            primary = protocol.settle((lambda: shutil.rmtree(work),), primary)
        return protocol.settle((control.check,), primary)


def run(
    options: protocol.Options, output: Path, descriptor: int, control: protocol.Control
) -> dict[str, object]:
    """Own the complete VM lifetime through every acquired peer's finalization."""
    pin = vm.CloudImage.read()
    report = receipt(options, pin)
    owners = Owners()
    primary: BaseException | None = None
    try:
        control.check()
        protocol.require(
            output.is_absolute() and output.resolve() == output and not output.exists()
        )
        vm.preflight(output)
        archive = owners.archive = Input(descriptor, options)
        archive.admit(control)
        output.mkdir(mode=0o700)
        host = Host(output, control)
        report["failure"] = "boot"
        work = owners.work = Path(tempfile.mkdtemp(prefix="ops-vm-private-", dir=output))
        with socket.socket() as selected:
            selected.bind(("127.0.0.1", 0))
            port = cast("tuple[str, int]", selected.getsockname())[1]
        vm.prepare_disk(host, work, pin)
        vm.create_seed(host, work, port, options.run_id)
        guest = owners.guest = vm.Guest(
            host, vm.qemu_command(work, port, options.run_id), qmp_path=work / "qmp.sock"
        )
        host.guest = guest
        report["guestExecuted"] = True
        vm.boot(host, guest, work, port)
        report["failure"] = "stage"
        stage(host, guest, work, port, options)
        report["failure"] = "cases"
        observed = exercise(host, guest, work, port, archive)
        report["cases"], report["install"] = observed["cases"], observed["install"]
        protocol.require(observed["passed"] is True)
        report["runtimeValidated"], report["installValidated"] = True, True
        archive.check(control)
        control.check()
        report["failure"] = None
    except BaseException as error:  # noqa: BLE001 -- Finalize every acquired peer before any return.
        primary = error
        control.status = control.status or 1
    finally:
        control.settling = True
        cleanup_error = owners.finish(control, report)
        if primary is None:
            primary = cleanup_error
        report["cleanupPassed"] = cleanup_error is None
        if cleanup_error is not None and report["failure"] is None:
            report["failure"] = "cleanup"
        if control.status == protocol.DEADLINE_STATUS or control.status in protocol.SIGNAL_STATUSES:
            report["failure"] = (
                "deadline" if control.status == protocol.DEADLINE_STATUS else "cancelled"
            )
        report["passed"] = (
            primary is None
            and control.status == 0
            and report["cleanupPassed"] is True
            and report["runtimeValidated"] is True
        )
    return report


def main(arguments: Sequence[str] | None = None) -> int:
    """Include receipt writing and the original deadline fence in the final status."""
    control = protocol.Control(SECONDS)
    old = {
        number: signal.signal(number, control.signal) for number in (signal.SIGINT, signal.SIGTERM)
    }
    try:
        parser = protocol.Parser(add_help=False, allow_abbrev=False)
        _ = parser.add_argument("--output", required=True, type=Path)
        parsed, remaining = parser.parse_known_args(
            sys.argv[1:] if arguments is None else arguments
        )
        output = cast("Path", parsed.output)
        options = protocol.Options.parse(remaining)
        report = run(options, output, sys.stdin.fileno(), control)
        data = json.dumps(report, separators=(",", ":")).encode() + b"\n"
        protocol.require(len(data) <= MAX_RECEIPT)
        if output.is_dir():
            write_private(output / "summary.json", data, mode=0o600)
        _ = sys.stdout.buffer.write(data)
        _ = sys.stdout.buffer.flush()
        if report["passed"] is True:
            control.check()
        return control.status or (0 if report["passed"] is True else 1)
    except (OSError, ValueError, ToolError, RecursionError):
        _ = sys.stderr.write("Ops Debian keeper validation is unproven.\n")
        return control.status or 1
    finally:
        for number, handler in old.items():
            _ = signal.signal(number, handler)


if __name__ == "__main__":
    sys.exit(main())
