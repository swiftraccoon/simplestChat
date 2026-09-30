"""Validate maintained Ansible configuration in one bounded, owned Debian KVM guest.

This controller accepts no host/inventory/provider selector. Its only SSH target
is the loopback port of the exact QEMU child it starts, authenticated by a fresh
host key placed in that guest's cloud-init seed. No production credentials or
agent are inherited. Only metadata-only summary.json is suitable for CI upload.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import selectors
import shlex
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, cast, final

from security_context import ROOT, environment, executable, json_object
from security_tools import ToolError, bounded_file, record, require, string, write_private
from security_vm_qmp import Observer

# isort: split
# security_context installs the canonical standalone operations-helper directory.
import bounded_process
from release_artifact import validate_manifest, verify_archive
from release_json import decode_json

if TYPE_CHECKING:
    from collections.abc import Sequence
    from types import FrameType
    from typing import BinaryIO, NoReturn

    from release_artifact import Manifest

GIB = 1024**3
MAX_IMAGE = 2 * GIB
MAX_STREAM = 2 * 1024**2
MAX_STARTUP_STDERR = 8192
MAX_STARTUP_LINES = 12
MAX_STARTUP_LINE = 1024
STARTUP_ASCII = frozenset(b"\t" + bytes(range(32, 127)))
MAX_PROC_REPORT = 65536
VM_SECONDS = 1800
VM_MEMORY_MIB = 3072
VM_DISK_GIB = 16
MAX_COMMANDS = 64
MAX_CONFIGURATION_FILES = 64
MAX_FIXTURE_ROWS = 16
MAX_MIGRATIONS = 10000
MIN_PORT = 1024
MAX_PORT = 65535
PIN = ROOT / "security/vm/debian-cloud.json"
RUN_ID = re.compile(r"[a-f0-9]{32}")
RECAP = re.compile(
    r"^vmfixture\s+:\s+ok=(\d+)\s+changed=(\d+)\s+unreachable=(\d+)"
    + r"\s+failed=(\d+)\s+skipped=(\d+)\s+rescued=(\d+)\s+ignored=(\d+)\s*$",
    re.MULTILINE,
)
STARTUP_ASSERTION = r"\bassertion\b.*\bfailed\b|\bcode should not be reached\b"
STARTUP_ERRORS = {
    "kvm": r"\bkvm\b|accelerator|accel=|hardware virtualization",
    "block_backend": r"backing|block node|blockdev|qcow2|image format|block driver",
    "backing_format": r"backing.*format|format.*backing|image format.*specified"
    + r"|auto-detect.*format",
    "image_access": r"could not (?:open|read)|failed to (?:open|lock)|read-only|read only",
    "sandbox": r"seccomp|sandbox|privilege|operation not permitted",
    "boot_device": r"boot(?:able)? device|bootindex|boot order|bios|firmware|romfile",
    "device": r"device|machine type|machine.*support|bus .*found|driver.*found",
    "memory": r"memory|mmap|address.space|allocation|allocate|ram size|pc\.ram",
    "resource_limit": r"rlimit|prlimit|resource limit|too many open files|file size limit",
    "thread_start": r"gthread|pthread|qemu_thread_create|creating thread|glib-error",
    "assertion_failure": STARTUP_ASSERTION,
    "network": r"network|netdev|host forwarding|hostfwd|address already in use|slirp",
    "option": r"invalid (?:option|parameter|argument)|unknown option"
    + r"|unrecognized option|expects|requires",
    "execution": r"failed to (?:execute|run)|command not found|exec format",
}
STARTUP_REASONS = (
    "permission denied",
    "operation not permitted",
    "cannot allocate memory",
    "invalid argument",
    "no such file or directory",
    "no such device",
    "not supported",
    "address already in use",
    "too many open files",
    "resource temporarily unavailable",
    "read-only",
)
STARTUP_COMPONENTS = (
    "rootdisk",
    "seed",
    "guest.qcow2",
    "base.qcow2",
    "seed.img",
    "pc.ram",
    "virtio-blk-pci",
    "virtio-net-pci",
    "q35",
    "kvm",
    "qcow2",
    "seccomp",
    "prlimit",
    "glib",
    "gthread",
    "pthread",
)
STARTUP_SOURCE_COMPONENTS = ("thread-pool.c", "qemu-thread-posix.c", "async.c", "gmem.c")
BOOT_MILESTONES = {
    "firmware": rb"seabios|tianocore|uefi firmware",
    "boot_disk": rb"booting from hard disk",
    "no_bootable_disk": rb"no bootable device|no bootable disk|boot failed",
    "grub": rb"\bgrub\b|booting `debian gnu/linux'",
    "linux": rb"linux version|booting linux",
    "kernel_panic": rb"kernel panic|not syncing",
    "initramfs": rb"\(initramfs\)|unable to mount root fs|dropping to a shell",
    "disk_resize": rb"growroot|growpart|resize2fs|partition resize",
    "reboot": rb"reboot: restarting system|rebooting",
    "poweroff": rb"power down|powering off",
    "cloud_init": rb"cloud-init",
    "ssh": rb"started openssh|starting openssh|ssh.service",
}
BOOT_OVERLAP = 128
MAX_CLOUD_STATUS = 65536
MAX_CLOUD_GROUPS = 17
MAX_CLOUD_ERRORS = 128
MAX_CLOUD_MESSAGE = 4096
MAX_CLOUD_TOTAL = 512
CLOUD_STAGES = ("init-local", "init", "modules-config", "modules-final")
CLOUD_RUNNING = ("not started", "running", "done", "disabled")
CLOUD_CLASSES = {
    "schema_validation": r"schema|invalid cloud.config|failed validating",
    "deprecated_config": r"deprecat",
    "hostname": r"hostname|host name",
    "apt": r"\bapt\b|deb822|sources\.list",
    "network": r"network|dhcp|dns|name resolution",
    "ssh": r"\bssh\b|ssh_|host key|authorized.key",
}


def cloud_errors(value: object) -> dict[str, object]:
    """Count bounded cloud-init errors and project only a fixed diagnostic vocabulary."""
    require(isinstance(value, dict), "vm_cloud_init_record")
    raw = cast("dict[str, object]", value)
    errors, recoverable = raw.get("errors"), raw.get("recoverable_errors")
    require(isinstance(errors, list) and isinstance(recoverable, dict), "vm_cloud_init_errors")
    groups = [cast("list[object]", errors), *cast("dict[str, object]", recoverable).values()]
    require(len(groups) <= MAX_CLOUD_GROUPS, "vm_cloud_init_error_limit")
    messages: list[str] = []
    for group in groups:
        require(
            isinstance(group, list) and len(cast("list[object]", group)) <= MAX_CLOUD_ERRORS,
            "vm_cloud_init_errors",
        )
        for message in cast("list[object]", group):
            require(
                isinstance(message, str) and len(message) <= MAX_CLOUD_MESSAGE,
                "vm_cloud_init_message",
            )
            messages.append(cast("str", message))
    require(len(messages) <= MAX_CLOUD_TOTAL, "vm_cloud_init_error_limit")
    classes: set[str] = set()
    unclassified = 0
    for message in messages:
        matched = {
            name for name, pattern in CLOUD_CLASSES.items() if re.search(pattern, message.lower())
        }
        classes.update(matched)
        unclassified += not matched
    return {
        "errors": len(cast("list[object]", errors)),
        "recoverableErrors": len(messages) - len(cast("list[object]", errors)),
        "failureClasses": sorted(classes),
        "unclassifiedErrors": unclassified,
    }


def cloud_status(data: bytes, exit_status: int) -> dict[str, object]:
    """Require completed healthy initialization; never export raw CLI fields or error messages."""
    require(len(data) <= MAX_CLOUD_STATUS, "vm_cloud_init_report_size")
    decoded = decode_json(data)
    require(isinstance(decoded, dict), "vm_cloud_init_record")
    raw = cast("dict[str, object]", decoded)
    require("stage" in raw, "vm_cloud_init_stage")
    status, extended, stage = raw.get("status"), raw.get("extended_status"), raw.get("stage")
    require(isinstance(status, str) and status in (*CLOUD_RUNNING, "error"), "vm_cloud_init_status")
    labels = (
        *CLOUD_RUNNING,
        "degraded done",
        "degraded running",
        *("error - " + name for name in CLOUD_RUNNING),
    )
    require(isinstance(extended, str) and extended in labels, "vm_cloud_init_status")
    require(
        stage is None or (isinstance(stage, str) and stage in CLOUD_STAGES), "vm_cloud_init_stage"
    )
    aggregate = cloud_errors(raw)
    stages = {name: cloud_errors(raw[name]) for name in CLOUD_STAGES if name in raw}
    healthy = all(
        item["errors"] == 0 and item["recoverableErrors"] == 0
        for item in [aggregate, *stages.values()]
    )
    return {
        "exitStatus": exit_status,
        "parsed": True,
        "status": status,
        "extendedStatus": extended,
        "stage": stage,
        "aggregate": aggregate,
        "stages": stages,
        "passed": exit_status == 0
        and status == extended == "done"
        and stage is None
        and len(stages) == len(CLOUD_STAGES)
        and healthy,
    }


def startup_errors(content: bytes) -> dict[str, object]:
    """Project only recognized emulator errors onto fixed public words, never raw stderr."""
    messages: list[dict[str, object]] = []
    withheld = 0
    lines = content.splitlines()
    for raw in lines[:MAX_STARTUP_LINES]:
        if len(raw) > MAX_STARTUP_LINE or any(byte not in STARTUP_ASCII for byte in raw):
            withheld += 1
            continue
        line = raw.decode("ascii").lower()
        if not line.startswith(
            (
                "qemu-system-x86_64:",
                "qemu:",
                "prlimit:",
                "timeout:",
                "could not access kvm kernel module:",
                "failed to initialize kvm:",
            )
        ) and not re.search(
            r"\b(?:glib-error|gthread|pthread_create)\b"
            + rf"|^(?:glib:)?error:.*(?:{STARTUP_ASSERTION})",
            line,
        ):
            withheld += 1
            continue
        classes = [name for name, pattern in STARTUP_ERRORS.items() if re.search(pattern, line)]
        if not classes:
            withheld += 1
            continue
        messages.append(
            {
                "classes": classes,
                "reasons": [reason for reason in STARTUP_REASONS if reason in line],
                "components": [name for name in STARTUP_COMPONENTS if name in line]
                + [
                    name
                    for name in STARTUP_SOURCE_COMPONENTS
                    if re.search(rf"(?:^|[/\s:]){re.escape(name)}:[0-9]+(?=[:\s]|$)", line)
                ],
            }
        )
    return {
        "messages": messages,
        "withheldLines": withheld + max(0, len(lines) - MAX_STARTUP_LINES),
    }


def terminal_status(pid: int) -> dict[str, object]:
    """Observe the exact owned leader without releasing its PID or process-group identity."""
    result = os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
    if result is None:
        return {"state": "running"}
    if result.si_code == os.CLD_EXITED:
        return {"state": "exited", "exitCode": result.si_status}
    require(result.si_code in (os.CLD_KILLED, os.CLD_DUMPED), "vm_terminal_status_unknown")
    return {
        "state": "signaled",
        "signal": result.si_status,
        "coreDumped": result.si_code == os.CLD_DUMPED,
    }


@dataclass(frozen=True)
class CloudImage:
    """One reviewed immutable upstream image identity, authenticated before parsing."""

    url: str
    sha512: str

    @classmethod
    def read(cls, path: Path = PIN) -> CloudImage:
        """Reject alternate releases, URLs or malformed checksum records."""
        raw = record(
            cast("object", json.loads(bounded_file(path, 16384))),
            {"schemaVersion", "distribution", "architecture", "url", "sha512", "checksumsUrl"},
        )
        url = string(raw["url"])
        require(
            type(raw["schemaVersion"]) is int
            and raw["schemaVersion"] == 1
            and raw["distribution"] == "Debian 13"
            and raw["architecture"] == "amd64"
            and re.fullmatch(
                r"https://cloud\.debian\.org/images/cloud/trixie/(\d{8}-\d+)/"
                + r"debian-13-genericcloud-amd64-\1\.qcow2",
                url,
            )
            and raw["checksumsUrl"] == url.rsplit("/", 1)[0] + "/SHA512SUMS"
            and re.fullmatch(r"[a-f0-9]{128}", string(raw["sha512"])),
            "invalid_vm_image_pin",
        )
        return cls(url, string(raw["sha512"]))


def verify_image(path: Path, expected: str) -> None:
    """Hash at most two GiB of an owned regular image without following a link."""
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as source:
        metadata = os.fstat(source.fileno())
        require(
            stat.S_ISREG(metadata.st_mode)
            and metadata.st_uid == os.getuid()
            and 0 < metadata.st_size <= MAX_IMAGE,
            "invalid_vm_image_file",
        )
        digest = hashlib.sha512()
        size = 0
        while chunk := source.read(min(1024**2, MAX_IMAGE + 1 - size)):
            size += len(chunk)
            require(size <= MAX_IMAGE, "vm_image_size_exceeded")
            digest.update(chunk)
        require(size == metadata.st_size and digest.hexdigest() == expected, "vm_image_digest")


def preflight(output: Path) -> None:
    """Require real local KVM and bounded disposable-runner resources; no emulation fallback."""
    require(
        platform.system() == "Linux" and platform.machine() == "x86_64", "vm_requires_linux_amd64"
    )
    kvm = Path("/dev/kvm")
    require(kvm.exists() and stat.S_ISCHR(kvm.stat().st_mode), "vm_kvm_missing")
    require(os.access(kvm, os.R_OK | os.W_OK), "vm_kvm_permission")
    require(os.geteuid() != 0, "vm_controller_must_be_unprivileged")
    require(shutil.disk_usage(output.parent).free >= 24 * GIB, "vm_disk_headroom")
    with Path("/proc/meminfo").open("rb") as source:
        memory = source.read(MAX_PROC_REPORT + 1)
    require(len(memory) <= MAX_PROC_REPORT, "vm_memory_report_size")
    memory = memory.decode()
    match = re.search(r"^MemAvailable:\s+(\d+) kB$", memory, re.MULTILINE)
    require(match is not None and int(match[1]) >= 5 * 1024**2, "vm_memory_headroom")
    for program in (
        "qemu-system-x86_64",
        "timeout",
        "qemu-img",
        "cloud-localds",
        "prlimit",
        "ssh",
        "ssh-keygen",
        "curl",
        "git",
    ):
        _ = executable(program)


@dataclass
class Host:
    """Own one private bounded command transcript and a shared wall-clock deadline."""

    output: Path
    deadline: float
    env: dict[str, str] = field(default_factory=dict)
    checks: list[dict[str, object]] = field(default_factory=list)

    def __post_init__(self) -> None:
        """Preserve required locale/tool paths without inheriting credentials or SSH policy."""
        self.env = environment(self.output)
        local_tmp = self.output / "ansible-local"
        local_tmp.mkdir(mode=0o700)
        self.env.update(
            {
                "ANSIBLE_CONFIG": str(ROOT / "ops/ansible/ansible.cfg"),
                "ANSIBLE_LOCAL_TEMP": str(local_tmp),
                "ANSIBLE_NOCOLOR": "1",
                "ANSIBLE_STDOUT_CALLBACK": "default",
                "ANSIBLE_LOAD_CALLBACK_PLUGINS": "0",
            }
        )
        tracking = os.environ.get("RUNNER_TRACKING_ID")
        if tracking is not None:
            require(re.fullmatch(r"[A-Za-z0-9_-]{1,128}", tracking), "invalid_vm_runner_tracking")
            self.env["RUNNER_TRACKING_ID"] = tracking

    def run(
        self,
        name: str,
        argv: Sequence[str],
        *,
        timeout: float = 120,
        accepted: tuple[int, ...] = (0,),
    ) -> tuple[int, bytes]:
        """Bound each command, reject unknown exits and expose only fixed failure codes."""
        require(re.fullmatch(r"[a-z0-9-]{1,64}", name), "invalid_vm_check_name")
        require(len(self.checks) < MAX_COMMANDS, "vm_command_budget")
        remaining = min(timeout, self.deadline - time.monotonic())
        require(remaining > 0, "vm_deadline")
        prefix = self.output / f"{len(self.checks):02d}-{name}"
        with (
            prefix.with_suffix(".stdout").open("xb") as stdout,
            prefix.with_suffix(".stderr").open("xb") as stderr,
        ):
            status, _, _ = bounded_process.run(
                argv,
                cwd=ROOT,
                env=self.env,
                output=stdout,
                error=stderr,
                limits=bounded_process.Limits(
                    timeout=remaining, stdout=MAX_STREAM, stderr=MAX_STREAM
                ),
            )
        self.checks.append({"name": name, "exitStatus": status})
        require(status in accepted, "vm_command_failed_" + name)
        return status, prefix.with_suffix(".stdout").read_bytes()


def ssh_options(work: Path) -> list[str]:
    """Authenticate only the generated guest; disable ambient agents, proxies and multiplexers."""
    options = [
        "BatchMode=yes",
        "IdentitiesOnly=yes",
        "IdentityAgent=none",
        "ForwardAgent=no",
        "PasswordAuthentication=no",
        "KbdInteractiveAuthentication=no",
        "StrictHostKeyChecking=yes",
        "GlobalKnownHostsFile=/dev/null",
        f"UserKnownHostsFile={work / 'known_hosts'}",
        "ProxyCommand=none",
        "ProxyJump=none",
        "ControlMaster=no",
        "ControlPath=none",
        "ControlPersist=no",
        "ConnectTimeout=5",
        "ConnectionAttempts=1",
        "ServerAliveInterval=5",
        "ServerAliveCountMax=2",
    ]
    return ["-F", "/dev/null", *(part for option in options for part in ("-o", option))]


def ssh_command(work: Path, port: int, command: Sequence[str]) -> list[str]:
    """Keep the only destination fixed to this VM's loopback-forwarded SSH port."""
    require(MIN_PORT <= port <= MAX_PORT, "invalid_vm_port")
    return [
        executable("ssh"),
        *ssh_options(work),
        "-i",
        str(work / "client"),
        "-p",
        str(port),
        "fixture@127.0.0.1",
        shlex.join(command),
    ]


def fixture_inventory(
    work: Path, port: int, run_id: str, artifacts: Path, revision: str
) -> dict[str, object]:
    """Generate a single-host static inventory with no external target or executable variables."""
    require(
        RUN_ID.fullmatch(run_id) and re.fullmatch(r"[a-f0-9]{40}", revision), "invalid_vm_identity"
    )
    require(MIN_PORT <= port <= MAX_PORT, "invalid_vm_port")
    return {
        "benchmark_hosts": {
            "hosts": {
                "vmfixture": {
                    "ansible_host": "127.0.0.1",
                    "ansible_port": port,
                    "ansible_user": "fixture",
                    "ansible_python_interpreter": "/usr/bin/python3",
                    "ansible_ssh_private_key_file": str(work / "client"),
                    "ansible_ssh_args": shlex.join(ssh_options(work)),
                    "ansible_become": True,
                    "scbench_revision": revision,
                    "scbench_upgrade_packages": False,
                    "scbench_reboot": False,
                    "scpub_enabled": True,
                    "scpub_release_revision": revision,
                    "scpub_domain": "vm-fixture.test",
                    "scpub_announce_ip": "127.0.0.1",
                    "scpub_announce_ipv6": "",
                    "scpub_media_workers": 1,
                    "scpub_app_cpus": 1,
                    "scpub_app_memory_mib": 1024,
                    "scpub_postgres_memory_mib": 1024,
                    "scpub_postgres_shared_buffers_mib": 256,
                    "scpub_backup_enabled": True,
                    "scpub_turn_enabled": False,
                    "vm_fixture_run_id": run_id,
                    "vm_fixture_artifact_dir": str(artifacts),
                }
            }
        }
    }


def create_seed(host: Host, work: Path, port: int, run_id: str) -> None:
    """Generate disposable client/host identities and a seed containing no real credentials."""
    for name in ("client", "host"):
        _ = host.run(
            "key-" + name,
            [
                executable("ssh-keygen"),
                "-q",
                "-t",
                "ed25519",
                "-N",
                "",
                "-C",
                "vm-fixture",
                "-f",
                str(work / name),
            ],
        )
    client_public = bounded_file(work / "client.pub", 4096).decode().strip()
    host_public = bounded_file(work / "host.pub", 4096).decode().strip()
    for key in (client_public, host_public):
        require(
            re.fullmatch(r"ssh-ed25519 [A-Za-z0-9+/=]+ vm-fixture", key), "invalid_vm_public_key"
        )
    config = {
        "hostname": "simplestchat-fixture",
        "ssh_pwauth": False,
        "disable_root": True,
        "ssh_keys": {
            "ed25519_private": bounded_file(work / "host", 8192).decode(),
            "ed25519_public": host_public,
        },
        "users": [
            {
                "name": "fixture",
                "lock_passwd": True,
                "shell": "/bin/bash",
                "sudo": ["ALL=(ALL) NOPASSWD:ALL"],
                "ssh_authorized_keys": [client_public],
            }
        ],
        "write_files": [
            {
                "path": "/etc/simplestchat-vm-fixture",
                "owner": "root:root",
                "permissions": "0600",
                "content": run_id + "\n",
            }
        ],
    }
    write_private(
        work / "user-data", b"#cloud-config\n" + json.dumps(config).encode() + b"\n", mode=0o600
    )
    write_private(
        work / "meta-data",
        json.dumps({"instance-id": run_id, "local-hostname": "simplestchat-fixture"}).encode(),
        mode=0o600,
    )
    write_private(work / "known_hosts", f"[127.0.0.1]:{port} {host_public}\n".encode(), mode=0o600)
    _ = host.run(
        "cloud-seed",
        [
            executable("cloud-localds"),
            str(work / "seed.img"),
            str(work / "user-data"),
            str(work / "meta-data"),
        ],
    )


def qemu_command(work: Path, port: int, run_id: str) -> list[str]:
    """Use KVM only, two vCPUs, finite memory/disk/CPU time and one loopback port."""
    require(RUN_ID.fullmatch(run_id) and MIN_PORT <= port <= MAX_PORT, "invalid_vm_identity")
    return [
        executable("timeout"),
        "--signal=TERM",
        "--kill-after=5s",
        str(VM_SECONDS) + "s",
        executable("prlimit"),
        f"--as={5 * GIB}",
        f"--fsize={17 * GIB}",
        "--cpu=3600",
        "--nofile=1024",
        "--nproc=512",
        "--core=0",
        "--",
        executable("qemu-system-x86_64"),
        "-name",
        "simplestchat-ci-" + run_id,
        "-no-user-config",
        "-nodefaults",
        "-machine",
        "q35,accel=kvm",
        "-cpu",
        "host",
        "-smp",
        "2",
        "-m",
        str(VM_MEMORY_MIB),
        "-display",
        "none",
        "-device",
        "VGA",
        "-monitor",
        "none",
        "-qmp",
        "unix:" + str(work / "qmp.sock").replace(",", ",,") + ",server=on,wait=on",
        "-serial",
        "stdio",
        "-sandbox",
        "on,obsolete=deny,elevateprivileges=deny,spawn=deny,resourcecontrol=deny",
        "-blockdev",
        json.dumps(
            {
                "driver": "qcow2",
                "node-name": "rootdisk",
                "file": {"driver": "file", "filename": str(work / "guest.qcow2")},
            }
        ),
        "-device",
        "virtio-blk-pci,drive=rootdisk",
        "-blockdev",
        json.dumps(
            {
                "driver": "raw",
                "node-name": "seed",
                "read-only": True,
                "file": {"driver": "file", "filename": str(work / "seed.img")},
            }
        ),
        "-device",
        "virtio-blk-pci,drive=seed",
        "-netdev",
        f"user,id=net0,hostfwd=tcp:127.0.0.1:{port}-:22",
        "-device",
        "virtio-net-pci,netdev=net0",
    ]


@final
class Guest:
    """Keep the exact QEMU leader unreaped until cleanup; never search by process name."""

    def __init__(self, host: Host, argv: Sequence[str], *, qmp_path: Path | None = None) -> None:
        """Start one owned process group and bounded pipe drainer without daemonization."""
        self.host = host
        self.failure: str | None = None
        self.drainer_errno: int | None = None
        self.drainer_status = "active"
        self.observed_terminal: dict[str, object] = {"state": "not_observed"}
        self.closed = False
        self.stopping = threading.Event()
        self.authenticated = threading.Event()
        self.startup_lock = threading.Lock()
        self.startup_buffer = bytearray()
        self.startup_bytes = 0
        self.startup_digest = hashlib.sha256()
        self.serial_bytes = 0
        self.serial_digest = hashlib.sha256()
        self.serial_tail = b""
        self.serial_milestones: set[str] = set()
        self.cloud_init: dict[str, object] | None = None
        self.observer = Observer(qmp_path, host.deadline) if qmp_path is not None else None
        self.child = subprocess.Popen(  # noqa: S603 -- Fixed reviewed QEMU/prlimit argv; no shell.
            argv,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=host.env,
            cwd=ROOT,
            start_new_session=True,
        )
        self.thread = threading.Thread(target=self.drain, name="owned-vm-output", daemon=True)
        try:
            self.thread.start()
        except BaseException:
            bounded_process.stop(self.child)
            raise
        if self.observer is not None:
            try:
                self.observer.start()
            except BaseException:
                self.close()
                raise

    def startup_capture(self, content: bytes) -> None:
        """Retain a finite emulator-only prefix until SSH authenticates this exact guest."""
        with self.startup_lock:
            if not self.authenticated.is_set():
                self.startup_bytes += len(content)
                self.startup_digest.update(content)
                self.startup_buffer.extend(
                    content[: max(0, MAX_STARTUP_STDERR - len(self.startup_buffer))]
                )

    def authenticated_ssh(self) -> None:
        """Close the public startup-diagnostic boundary before cloud-init or Ansible runs."""
        with self.startup_lock:
            self.authenticated.set()
            self.startup_buffer.clear()
            self.serial_tail = b""
            self.serial_milestones.clear()

    def serial_capture(self, content: bytes) -> None:
        """Retain only fixed boot milestones from bounded pre-auth guest serial output."""
        with self.startup_lock:
            if not self.authenticated.is_set():
                self.serial_bytes += len(content)
                self.serial_digest.update(content)
                window = (self.serial_tail + content).lower()
                for name, pattern in BOOT_MILESTONES.items():
                    if re.search(pattern, window):
                        self.serial_milestones.add(name)
                self.serial_tail = window[-BOOT_OVERLAP:]

    def diagnostics(self) -> dict[str, object]:
        """Publish numeric lifecycle facts and a finite stderr vocabulary, never exception text."""
        result: dict[str, object] = {
            "terminalBeforeCleanup": self.observed_terminal,
            "cleanupReturnCode": self.child.returncode,
            "drainer": self.drainer_status,
            "drainerErrno": self.drainer_errno,
            "authenticatedSsh": self.authenticated.is_set(),
        }
        if self.observer is not None:
            result["qmp"] = self.observer.diagnostics()
        if self.cloud_init is not None:
            result["cloudInit"] = self.cloud_init
        with self.startup_lock:
            if not self.authenticated.is_set():
                result["startupStderr"] = {
                    "bytesObserved": self.startup_bytes,
                    "sha256": self.startup_digest.hexdigest(),
                    "prefixBytes": len(self.startup_buffer),
                    "truncated": self.startup_bytes > len(self.startup_buffer),
                    **startup_errors(bytes(self.startup_buffer)),
                }
                result["startupSerial"] = {
                    "bytesObserved": self.serial_bytes,
                    "sha256": self.serial_digest.hexdigest(),
                    "milestones": sorted(self.serial_milestones),
                }
        return result

    def drain(self) -> None:
        """Bound VM output while preserving the child PID for exact process-group cleanup."""
        try:
            with (
                selectors.DefaultSelector() as selector,
                (self.host.output / "qemu.stdout").open("xb") as stdout,
                (self.host.output / "qemu.stderr").open("xb") as stderr,
            ):
                require(
                    self.child.stdout is not None and self.child.stderr is not None,
                    "vm_pipe_missing",
                )
                counts: dict[int, int] = {}
                destinations = {"stdout": stdout, "stderr": stderr}
                for stream, name in (
                    (self.child.stdout, "stdout"),
                    (self.child.stderr, "stderr"),
                ):
                    descriptor = cast("BinaryIO", stream).fileno()
                    os.set_blocking(descriptor, False)
                    _ = selector.register(descriptor, selectors.EVENT_READ, name)
                    counts[descriptor] = 0
                while selector.get_map() and not self.stopping.is_set():
                    require(time.monotonic() < self.host.deadline, "vm_deadline")
                    for key, _events in selector.select(0.1):
                        content = os.read(key.fd, 65536)
                        if not content:
                            _ = selector.unregister(key.fd)
                            continue
                        name = cast("str", key.data)
                        if name == "stderr":
                            self.startup_capture(content)
                        else:
                            self.serial_capture(content)
                        require(
                            counts[key.fd] + len(content) <= MAX_STREAM, "vm_" + name + "_limit"
                        )
                        counts[key.fd] += len(content)
                        _ = destinations[name].write(content)
                self.drainer_status = "stopped" if self.stopping.is_set() else "completed"
        except ToolError as error:
            known = {"vm_deadline", "vm_stdout_limit", "vm_stderr_limit", "vm_pipe_missing"}
            self.failure = str(error) if str(error) in known else "vm_drainer_policy_error"
            self.drainer_status = self.failure
        except OSError as error:
            self.failure = self.drainer_status = "vm_drainer_io_error"
            self.drainer_errno = error.errno
        except BaseException:  # noqa: BLE001 -- Thread boundary must fail closed on any failure.
            self.failure = self.drainer_status = "vm_drainer_unexpected_error"

    def require_alive(self) -> None:
        """Observe exit without poll()/wait(), keeping the owned process group reserved."""
        self.observed_terminal = terminal_status(self.child.pid)
        if self.observer is not None and self.observer.failure is not None:
            self.failure = self.observer.failure
        require(self.failure is None, self.failure or "vm_drainer_unhealthy")
        require(self.observed_terminal["state"] == "running", "vm_child_exited")

    def close(self) -> None:
        """Stop only this unreaped child's group and close its private output pipes."""
        if self.closed:
            return
        try:
            self.observed_terminal = terminal_status(self.child.pid)
            bounded_process.stop(self.child)
            self.closed = True
        finally:
            self._close_io()
        if self.failure is None and self.drainer_status != "completed":
            self.failure = self.drainer_status = "vm_output_incomplete"
        if self.observer is not None and self.observer.failure is not None:
            self.failure = self.failure or self.observer.failure
            require(self.observer.failure != "vm_qmp_cleanup_incomplete", self.observer.failure)

    def _close_io(self) -> None:
        """Stop diagnostic readers even when the exact owned-child cleanup fails."""
        try:
            if self.observer is not None:
                self.observer.close()
        finally:
            # Cleanup closes every owned producer. Let the drainer consume its final
            # error bytes through EOF before requesting a bounded forced stop.
            self.thread.join(timeout=1)
            self.stopping.set()
            self.thread.join(timeout=5)
            require(not self.thread.is_alive(), "vm_output_cleanup_incomplete")
            for stream in (self.child.stdout, self.child.stderr):
                if stream is not None:
                    stream.close()


def recap(data: bytes, *, unchanged: bool) -> dict[str, int]:
    """Require one complete successful Ansible recap; skipped rescue/failures cannot pass."""
    matches = list(RECAP.finditer(data.decode()))
    require(len(matches) == 1, "vm_ansible_recap_missing_or_ambiguous")
    require(
        re.findall(r"^(\S+)\s+:\s+ok=", data.decode(), re.MULTILINE) == ["vmfixture"],
        "vm_ansible_unexpected_host",
    )
    names = ("ok", "changed", "unreachable", "failed", "skipped", "rescued", "ignored")
    result: dict[str, int] = dict(zip(names, map(int, matches[0].groups()), strict=True))
    require(
        result["ok"] > 0
        and all(result[name] == 0 for name in ("unreachable", "failed", "rescued", "ignored")),
        "vm_ansible_incomplete",
    )
    require(not unchanged or result["changed"] == 0, "vm_ansible_not_idempotent")
    return result


def playbook(host: Host, inventory: Path, name: str, *, second: bool = False) -> dict[str, int]:
    """Run the maintained playbook against only the generated fixture host."""
    selected = {
        "site": "ops/ansible/site.yml",
        "stage": "security/vm/stage.yml",
        "public": "ops/ansible/public.yml",
        "backup": "ops/ansible/backup.yml",
    }
    require(name in selected, "unknown_vm_playbook")
    args = [
        sys.executable,
        "-m",
        "ansible.cli.playbook",
        "-i",
        str(inventory),
        "--limit",
        "vmfixture",
        "--forks",
        "1",
        selected[name],
    ]
    if name == "site":
        args.extend(["--tags", "host,docker,benchmark"])
    _, output = host.run(name + ("-second" if second else "-first"), args, timeout=600)
    return recap(output, unchanged=second)


def guest_action(host: Host, work: Path, port: int, run_id: str, action: str) -> dict[str, object]:
    """Accept a narrow success receipt, never arbitrary remote command output."""
    require(action in ("snapshot", "database", "backup-restore"), "unknown_vm_action")
    _, output = host.run(
        action,
        ssh_command(
            work,
            port,
            [
                "sudo",
                "-n",
                "/usr/bin/python3",
                "-I",
                "/root/simplestchat-vm-fixture/security_vm_guest.py",
                action,
                "--run-id",
                run_id,
            ],
        ),
        timeout=400,
    )
    value = json_object(output)
    require(
        type(value.get("schemaVersion")) is int
        and value.get("schemaVersion") == 1
        and value.get("runId") == run_id
        and value.get("action") == action
        and value.get("passed") is True,
        "invalid_vm_guest_receipt",
    )
    common = {"schemaVersion", "runId", "action", "passed"}
    extras: dict[str, set[str]] = {
        "snapshot": {"configurationFiles", "idempotent"},
        "database": {"migrations", "ready", "counts"},
        "backup-restore": {"archiveValidated", "restoreVerified", "cleanupPassed", "counts"},
    }
    require(set(value) == common | extras[action], "invalid_vm_guest_receipt_fields")
    if action == "snapshot":
        require(
            type(value["configurationFiles"]) is int
            and 0 < value["configurationFiles"] <= MAX_CONFIGURATION_FILES
            and type(value["idempotent"]) is bool,
            "invalid_vm_snapshot_receipt",
        )
    else:
        counts = record(
            value["counts"],
            {"users", "rooms", "sessions", "credentials", "activeIncidents", "resolvedIncidents"},
        )
        require(
            all(type(count) is int and 0 <= count <= MAX_FIXTURE_ROWS for count in counts.values()),
            "invalid_vm_fixture_counts",
        )
        if action == "database":
            require(
                type(value["migrations"]) is int
                and 0 < value["migrations"] <= MAX_MIGRATIONS
                and value["ready"] is True,
                "invalid_vm_database_receipt",
            )
        else:
            require(
                all(value[key] is True for key in extras[action] - {"counts"}),
                "invalid_vm_restore_receipt",
            )
    return {"action": action, "passed": True, **{key: value[key] for key in extras[action]}}


def prepare_disk(host: Host, work: Path, pin: CloudImage) -> None:
    """Authenticate downloaded bytes before the QEMU image parser or guest starts."""
    image = work / "base.qcow2"
    _ = host.run(
        "download-cloud",
        [
            executable("curl"),
            "--disable",
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--connect-timeout",
            "15",
            "--max-time",
            "300",
            "--max-filesize",
            str(MAX_IMAGE),
            "--output",
            str(image),
            pin.url,
        ],
        timeout=310,
    )
    verify_image(image, pin.sha512)
    _, info = host.run(
        "cloud-image-info", [executable("qemu-img"), "info", "--output=json", str(image)]
    )
    value = json_object(info)
    size = value.get("virtual-size")
    require(
        value.get("format") == "qcow2"
        and type(size) is int
        and 0 < size <= VM_DISK_GIB * GIB
        and not value.get("backing-filename")
        and not value.get("snapshots"),
        "invalid_vm_disk_layout",
    )
    _ = host.run(
        "cloud-overlay",
        [
            executable("qemu-img"),
            "create",
            "-f",
            "qcow2",
            "-F",
            "qcow2",
            "-b",
            str(image),
            str(work / "guest.qcow2"),
            f"{VM_DISK_GIB}G",
        ],
    )


def boot(host: Host, guest: Guest, work: Path, port: int) -> None:
    """Wait only for this authenticated guest's initial SSH readiness, with a fixed budget."""
    for _ in range(24):
        guest.require_alive()
        status, _ = host.run(
            "ssh-ready", ssh_command(work, port, ["/usr/bin/true"]), timeout=8, accepted=(0, 255)
        )
        if status == 0:
            guest.authenticated_ssh()
            break
        time.sleep(2)
    else:
        message = "vm_boot_timeout"
        raise ToolError(message)
    status, data = host.run(
        "cloud-init",
        ssh_command(work, port, ["sudo", "-n", "cloud-init", "status", "--wait", "--format=json"]),
        timeout=180,
        accepted=(0, 1, 2),
    )
    guest.cloud_init = {
        "exitStatus": status,
        "parsed": False,
        "failure": "vm_cloud_init_report_invalid",
    }
    try:
        guest.cloud_init = cloud_status(data, status)
    except (ValueError, RecursionError) as error:
        message = "vm_cloud_init_report_invalid"
        raise ToolError(message) from error
    require(guest.cloud_init["passed"] is True, "vm_cloud_init_unhealthy")
    guest.require_alive()


def exercise(host: Host, guest: Guest, work: Path, port: int, run_id: str) -> dict[str, object]:
    """Apply real configuration twice, then validate the real local backup/restore path."""
    inventory = work / "inventory.json"
    phases: dict[str, object] = {}
    for second in (False, True):
        guest.require_alive()
        phases["siteSecond" if second else "siteFirst"] = playbook(
            host, inventory, "site", second=second
        )
    phases["stage"] = playbook(host, inventory, "stage")
    for second in (False, True):
        guest.require_alive()
        phases["publicSecond" if second else "publicFirst"] = playbook(
            host, inventory, "public", second=second
        )
        phases["snapshotSecond" if second else "snapshotFirst"] = guest_action(
            host, work, port, run_id, "snapshot"
        )
    phases["database"] = guest_action(host, work, port, run_id, "database")
    for second in (False, True):
        guest.require_alive()
        phases["backupSecond" if second else "backupFirst"] = playbook(
            host, inventory, "backup", second=second
        )
    phases["restore"] = guest_action(host, work, port, run_id, "backup-restore")
    guest.require_alive()
    return phases


def source_revision(host: Host) -> str:
    """Require a clean source tree for either the deployment or explicit boot-only scope."""
    _, revision = host.run("source-revision", [executable("git"), "rev-parse", "HEAD"])
    selected = revision.decode().strip()
    require(re.fullmatch(r"[a-f0-9]{40}", selected), "vm_invalid_source_revision")
    _, changes = host.run(
        "source-clean", [executable("git"), "status", "--porcelain", "--untracked-files=normal"]
    )
    require(not changes.strip(), "vm_requires_clean_checkout")
    return selected


def artifact_input(host: Host, path: Path) -> Manifest:
    """Require the canonical current-head artifact before copying anything into the guest."""
    require(
        path.is_absolute() and path.resolve() == path and path.is_dir(),
        "invalid_vm_artifact_directory",
    )
    metadata = path.stat()
    require(
        metadata.st_uid == os.getuid() and not metadata.st_mode & 0o022,
        "unprotected_vm_artifact_directory",
    )
    for name, maximum in (("release.json", 1024**2), ("image.tar", MAX_IMAGE)):
        entry = path / name
        metadata = entry.lstat()
        require(
            stat.S_ISREG(metadata.st_mode)
            and metadata.st_uid == os.getuid()
            and not metadata.st_mode & 0o022
            and 0 < metadata.st_size <= maximum,
            "invalid_vm_release_file",
        )
    manifest = validate_manifest(path / "release.json")
    _ = verify_archive(path / "image.tar", manifest)
    require(source_revision(host) == manifest["revision"], "vm_release_revision_differs")
    return manifest


def inputs(host: Host, artifact_dir: Path | None, *, boot_only: bool) -> dict[str, object]:
    """Boot diagnostics cannot select an application artifact or claim its verification."""
    if boot_only:
        require(artifact_dir is None, "vm_boot_only_forbids_artifact")
        return {"revision": source_revision(host)}
    require(artifact_dir is not None, "vm_deployment_requires_artifact")
    manifest = artifact_input(host, cast("Path", artifact_dir))
    return {"revision": manifest["revision"], "imageArchiveSha256": manifest["archiveSha256"]}


def inventory(work: Path, port: int, run_id: str, artifact: Path | None, revision: str) -> None:
    """Create an Ansible inventory only for the canonical deployment scope."""
    if artifact is not None:
        write_private(
            work / "inventory.json",
            json.dumps(fixture_inventory(work, port, run_id, artifact, revision)).encode(),
            mode=0o600,
        )


def close_guest(guest: Guest, report: dict[str, object]) -> None:
    """Retain exact lifecycle evidence even when owned-process cleanup itself fails."""
    try:
        guest.close()
    finally:
        report["vmLifecycle"] = guest.diagnostics()


def run(artifact_dir: Path | None, output: Path, *, boot_only: bool = False) -> dict[str, object]:
    """Publish success only after guest assertions and exact owned-process cleanup succeed."""
    require(output.is_absolute() and output.resolve() == output, "invalid_vm_output")
    output.mkdir(mode=0o700)
    host = Host(output, time.monotonic() + VM_SECONDS)
    run_id = uuid.uuid4().hex
    report: dict[str, object] = {
        "schemaVersion": 1,
        "runId": run_id,
        "passed": False,
        "cleanupPassed": False,
        "guestExecuted": False,
        "bootOnly": boot_only,
        "scope": "boot" if boot_only else "deployment-restore",
        "fullDeploymentValidated": False,
        "checks": host.checks,
    }
    guest: Guest | None = None
    work: Path | None = None
    try:
        preflight(output)
        pin = CloudImage.read()
        report.update(inputs(host, artifact_dir, boot_only=boot_only))
        report["cloudImageSha512"] = pin.sha512
        work = Path(tempfile.mkdtemp(prefix="vm-private-", dir=output))
        with socket.socket() as selected:
            selected.bind(("127.0.0.1", 0))
            port = cast("tuple[str, int]", selected.getsockname())[1]
        prepare_disk(host, work, pin)
        create_seed(host, work, port, run_id)
        inventory(work, port, run_id, artifact_dir, string(report["revision"]))
        guest = Guest(host, qemu_command(work, port, run_id), qmp_path=work / "qmp.sock")
        report["guestExecuted"] = True
        boot(host, guest, work, port)
        if not boot_only:
            report["phases"] = exercise(host, guest, work, port, run_id)
        report["passed"] = True
    except BaseException as error:
        report["failure"] = str(error) if isinstance(error, ToolError) else type(error).__name__
        raise
    finally:
        try:
            if guest is not None:
                close_guest(guest, report)
            if work is not None:
                shutil.rmtree(work)
            report["cleanupPassed"] = True
        except BaseException:
            report["passed"] = False
            report["cleanupFailure"] = "vm_owned_cleanup_failed"
            raise
        finally:
            if guest is not None and guest.failure is not None:
                report["passed"] = False
                _ = report.setdefault("failure", guest.failure)
            report["fullDeploymentValidated"] = report["passed"] is True and not boot_only
            write_private(
                output / "summary.json", json.dumps(report, indent=2).encode() + b"\n", mode=0o600
            )
    if guest is not None:
        require(guest.failure is None, guest.failure or "vm_drainer_unhealthy")
    return report


class Options(argparse.Namespace):
    """There are deliberately no provider, host, inventory or arbitrary-command options."""

    artifact_dir: Path | None = None
    boot_only: bool = False
    output: Path = Path()


def main() -> int:
    """Keep public diagnostics fixed while retaining bounded private failure evidence."""
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    _ = mode.add_argument("--artifact-dir", type=Path)
    _ = mode.add_argument("--boot-only", action="store_true")
    _ = parser.add_argument("--output", type=Path, required=True)
    options = parser.parse_args(namespace=Options())
    _ = os.umask(0o077)

    def interrupted(_signal: int, _frame: FrameType | None) -> NoReturn:
        message = "vm_interrupted"
        raise ToolError(message)

    for number in (signal.SIGTERM, signal.SIGINT):
        _ = signal.signal(number, interrupted)
    try:
        _ = run(
            options.artifact_dir.absolute() if options.artifact_dir is not None else None,
            options.output.absolute(),
            boot_only=options.boot_only,
        )
    except (ToolError, OSError, ValueError, RuntimeError):
        _ = sys.stderr.write("Disposable VM validation failed; inspect its private evidence.\n")
        return 1
    scope = "boot-only" if options.boot_only else "deployment and restore"
    _ = sys.stdout.write(f"Disposable VM {scope} validation and owned cleanup passed.\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
