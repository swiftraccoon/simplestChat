"""Enroll trusted SSH access and prepare one private Debian benchmark host.

Passwords are accepted only through a protected file or a terminal prompt.
The controller preserves existing authentication, never accepts an unverified
host key, and delegates provisioning to the checkout's canonical site.yml.
"""

# Fixed error codes prevent accidental disclosure of credentials or child output.
# ruff: noqa: EM101

from __future__ import annotations

import argparse
import base64
import contextlib
import getpass
import hashlib
import json
import os
import re
import resource
import secrets
import shlex
import stat
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, cast

import release_build as BUILD  # noqa: N812 -- Shared bounded subprocess boundary.
import yaml
from bootstrap_access import ENROLLED, BootstrapError, password_session, require
from release_json import JsonObject, decode_json, object_value

if TYPE_CHECKING:
    from collections.abc import Sequence

ROOT = Path(__file__).resolve().parents[1]
MAX_PRIVATE_BYTES = 65536
MINIMUM_FREE_GIB = 20
GIB = 1024**3
PRIVATE_MODE = 0o600
MAX_PASSWORD_CHARACTERS = 1024
MAX_KEY_BYTES = 4096
DEFAULT_SSH_PORT = 22
MAX_PORT = 65535
MAX_MINIMUM_DISK_GIB = 1000
HOST_KEY_FIELDS = 3


@dataclass(frozen=True)
class BootstrapTarget:
    """Explicit SSH endpoint and local identity/trust paths; no credentials."""

    host: str
    user: str
    port: int
    identity: Path
    known_hosts: Path


@dataclass
class BootstrapOptions(argparse.Namespace):
    """Public target selectors and opt-in canonical host maintenance."""

    host: str = ""
    user: str = "debian"
    port: int = 22
    name: str = ""
    revision: str = ""
    identity: str = ""
    known_hosts: str = "~/.ssh/known_hosts"
    host_fingerprint: str | None = None
    inventory: str = ""
    output: str = ""
    password_file: str | None = None
    replacement_password_file: str | None = None
    become_password_file: str | None = None
    minimum_free_gib: int = MINIMUM_FREE_GIB
    provision: bool = False
    initial_maintenance: bool = False


def run_command(argv: Sequence[str], *, timeout: float = 30) -> tuple[int, str]:
    """Run public arguments with bounded, unlogged output and fixed errors."""
    try:
        command = list(argv)
        if command[0] in ("ssh", "ssh-keygen", "ssh-keyscan", "git"):
            command[0] = "/usr/bin/" + command[0]
        return BUILD.Runner().run(
            command, cwd=ROOT, timeout=timeout, allow_failure=True, env=controller_environment()
        )
    except (BUILD.BuildError, OSError, UnicodeError):
        raise BootstrapError("controller_command_failed") from None


def ssh_options(target: BootstrapTarget) -> list[str]:
    """Pin direct OpenSSH transport, identity, host trust and bounded liveness."""
    return [
        "-F", "/dev/null", "-p", str(target.port), "-i", str(target.identity),
        "-o", "IdentitiesOnly=yes", "-o", "IdentityAgent=none",
        "-o", "ForwardAgent=no", "-o", "ClearAllForwardings=yes",
        "-o", "ControlMaster=no", "-o", "ControlPath=none",
        "-o", "StrictHostKeyChecking=yes", "-o", "UpdateHostKeys=no",
        "-o", "GlobalKnownHostsFile=/dev/null",
        "-o", f"UserKnownHostsFile={target.known_hosts}",
        "-o", "ConnectTimeout=15", "-o", "ConnectionAttempts=1",
        "-o", "ServerAliveInterval=10", "-o", "ServerAliveCountMax=3",
    ]  # fmt: skip


def ssh_command(
    target: BootstrapTarget, command: str, *, batch: bool = True, tty: bool = False
) -> list[str]:
    """Construct one direct SSH command without interpreting user shell syntax."""
    authentication = (
        ["-o", "BatchMode=yes", "-o", "PreferredAuthentications=publickey",
         "-o", "PasswordAuthentication=no", "-o", "KbdInteractiveAuthentication=no"]
        if batch else
        ["-o", "BatchMode=no", "-o", "PreferredAuthentications=password",
         "-o", "PubkeyAuthentication=no", "-o", "KbdInteractiveAuthentication=no",
         "-o", "NumberOfPasswordPrompts=1"]
    )  # fmt: skip
    return [
        "/usr/bin/ssh", *ssh_options(target), *authentication, "-tt" if tty else "-T",
        f"{target.user}@{target.host}", command,
    ]  # fmt: skip


def controller_environment() -> dict[str, str]:
    """Exclude inherited interpreters, loaders, plugins, agents and shell hooks."""
    return {
        "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
        "HOME": str(Path.home()),
        "LC_ALL": "C",
        "LANG": "C",
    }


def ansible_environment(root: Path) -> dict[str, str]:
    """Select the canonical configuration without inherited Ansible overrides."""
    result = controller_environment()
    result["PATH"] = str(root / "ops/ansible/.venv/bin") + ":" + result["PATH"]
    result.update(
        ANSIBLE_CONFIG=str(root / "ops/ansible/ansible.cfg"),
        ANSIBLE_HOST_KEY_CHECKING="True",
        ANSIBLE_RETRY_FILES_ENABLED="False",
    )
    return result


def site_command(  # noqa: PLR0913 -- Explicit shared controller boundary.
    root: Path,
    inventory: Path,
    name: str,
    revision: str,
    *,
    maintenance: bool = False,
    ask_become_pass: bool = False,
) -> list[str]:
    """Advance the explicit source revision without editing stored inventory flags."""
    require(re.fullmatch(r"[a-f0-9]{40}", revision), "invalid_revision")
    require(re.fullmatch(r"[A-Za-z][A-Za-z0-9_-]{0,62}", name), "invalid_inventory_name")
    return [
        str(root / "ops/ansible/.venv/bin/ansible-playbook"),
        "-i", str(inventory), str(root / "ops/ansible/site.yml"), "--limit", name,
        "--extra-vars", json.dumps({
            "scbench_revision": revision,
            "scbench_upgrade_packages": maintenance,
            "scbench_reboot": maintenance,
        }),
        *(["--ask-become-pass"] if ask_become_pass else []),
    ]  # fmt: skip


def local_path(value: str) -> Path:
    """Resolve a path without accepting a symlink as the selected file itself."""
    path = Path(value).expanduser().absolute()
    require(not path.is_symlink(), "selected_path_is_symlink")
    return path.resolve()


def outside_checkout(path: Path, root: Path) -> None:
    """Keep private authentication material entirely outside the checkout."""
    require(not path.is_relative_to(root.resolve()), "credential_path_inside_checkout")


def protected_file(path: Path, *, secret: bool = True) -> bytes:
    """Read only a bounded, caller-owned regular file with suitable permissions."""
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        metadata = os.fstat(descriptor)
        require(stat.S_ISREG(metadata.st_mode), "invalid_private_file_type")
        require(metadata.st_uid == os.getuid(), "invalid_private_file_owner")
        mode = stat.S_IMODE(metadata.st_mode)
        require(mode == PRIVATE_MODE if secret else mode & 0o022 == 0, "invalid_private_file_mode")
        require(0 < metadata.st_size <= MAX_PRIVATE_BYTES, "invalid_private_file_size")
        result = os.read(descriptor, MAX_PRIVATE_BYTES + 1)
        require(len(result) <= MAX_PRIVATE_BYTES, "invalid_private_file_size")
        return result
    finally:
        os.close(descriptor)


def private_parent(path: Path) -> None:
    """Require an existing caller-owned parent that other users cannot modify."""
    metadata = path.parent.stat()
    require(stat.S_ISDIR(metadata.st_mode), "invalid_private_parent")
    require(metadata.st_uid == os.getuid(), "invalid_private_parent_owner")
    require(stat.S_IMODE(metadata.st_mode) & 0o022 == 0, "unsafe_private_parent_mode")


def write_new(path: Path, data: bytes) -> None:
    """Create a private file exclusively; never replace an operator's existing file."""
    private_parent(path)
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        with os.fdopen(descriptor, "wb", closefd=False) as output:
            _ = output.write(data)
            output.flush()
            os.fsync(descriptor)
    finally:
        os.close(descriptor)
    sync_parent(path)


def sync_parent(path: Path) -> None:
    """Make a newly published recovery or key directory entry crash-durable."""
    descriptor = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def sync_file(path: Path) -> None:
    """Flush a validated key before its public half is enrolled remotely."""
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        require(stat.S_ISREG(os.fstat(descriptor).st_mode), "invalid_key_type")
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    sync_parent(path)


def write_record(path: Path, value: JsonObject) -> None:
    """Retain a fresh non-secret evidence record with private permissions."""
    write_new(path, (json.dumps(value, indent=2) + "\n").encode())


def password_value(path: Path | None, prompt: str, root: Path) -> str:
    """Read a plain password or a private JSON object's password field, never argv."""
    if path is None:
        require(sys.stdin.isatty(), "password_prompt_requires_terminal")
        value = getpass.getpass(prompt)
    else:
        outside_checkout(path, root)
        text = protected_file(path).decode("utf-8")
        value = text.rstrip("\r\n")
        if text.lstrip().startswith("{"):
            decoded = object_value(decode_json(text)).get("password")
            require(isinstance(decoded, str), "invalid_password_file")
            if not isinstance(decoded, str):
                raise BootstrapError("invalid_password_file")
            value = decoded
    require(1 <= len(value) <= MAX_PASSWORD_CHARACTERS, "invalid_password_length")
    require(not any(char in value for char in "\x00\r\n"), "invalid_password_characters")
    return value


def replacement_password(path: Path, root: Path) -> str:
    """Save a replacement before contacting SSH, so interrupted rotation is recoverable."""
    outside_checkout(path, root)
    if path.exists():
        return password_value(path, "", root)
    value = secrets.token_urlsafe(36)
    write_record(path, {"password": value})
    return value


def key_fingerprint(encoded: str) -> str:
    """Compute OpenSSH's SHA256 public-key fingerprint from a validated key blob."""
    try:
        blob = base64.b64decode(encoded, validate=True)
    except ValueError:
        raise BootstrapError("invalid_host_key") from None
    require(len(blob) <= MAX_KEY_BYTES, "invalid_host_key")
    digest = base64.b64encode(hashlib.sha256(blob).digest()).decode().rstrip("=")
    return "SHA256:" + digest


def host_token(target: BootstrapTarget) -> str:
    """Return OpenSSH's known_hosts selector for the chosen port."""
    return target.host if target.port == DEFAULT_SSH_PORT else f"[{target.host}]:{target.port}"


def host_trust(target: BootstrapTarget, fingerprint: str | None) -> str:
    """Use existing strict trust, or append only an independently pinned Ed25519 key."""
    private_parent(target.known_hosts)
    if target.known_hosts.exists():
        _ = protected_file(target.known_hosts, secret=False)
        status, text = run_command(
            ["ssh-keygen", "-F", host_token(target), "-f", str(target.known_hosts)]
        )
        if status == 0:
            keys = [line.split() for line in text.splitlines() if not line.startswith("#")]
            hashes = {key_fingerprint(parts[2]) for parts in keys if len(parts) >= HOST_KEY_FIELDS}
            require(bool(hashes), "invalid_known_host_entry")
            require(fingerprint is None or fingerprint in hashes, "host_fingerprint_mismatch")
            return fingerprint or "existing_known_hosts"
    require(fingerprint is not None, "verified_host_fingerprint_required")
    if fingerprint is None:
        raise BootstrapError("verified_host_fingerprint_required")
    status, text = run_command(
        ["ssh-keyscan", "-T", "10", "-p", str(target.port), "-t", "ed25519", target.host],
        timeout=30,
    )
    require(status == 0, "host_key_scan_failed")
    candidates = {
        (parts[1], parts[2])
        for line in text.splitlines()
        if not line.startswith("#") and len(parts := line.split()) == HOST_KEY_FIELDS
    }
    require(len(candidates) == 1, "ambiguous_host_key_scan")
    kind, encoded = next(iter(candidates))
    require(
        kind == "ssh-ed25519" and key_fingerprint(encoded) == fingerprint,
        "host_fingerprint_mismatch",
    )
    entry = f"\n{host_token(target)} {kind} {encoded}\n".encode()
    if target.known_hosts.exists():
        descriptor = os.open(target.known_hosts, os.O_WRONLY | os.O_APPEND | os.O_NOFOLLOW)
        try:
            _ = os.write(descriptor, entry)
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    else:
        write_new(target.known_hosts, entry)
    return fingerprint


def ensure_identity(target: BootstrapTarget, name: str) -> str:
    """Create or verify the dedicated unencrypted automation key without replacing keys."""
    outside_checkout(target.identity, ROOT)
    private_parent(target.identity)
    if not target.identity.exists():
        public_path = target.identity.with_suffix(target.identity.suffix + ".pub")
        require(
            not public_path.exists() and not public_path.is_symlink(),
            "orphan_public_key_exists",
        )
        status, _ = run_command(
            [
                "ssh-keygen",
                "-q",
                "-t",
                "ed25519",
                "-N",
                "",
                "-f",
                str(target.identity),
                "-C",
                f"simplestchat-bootstrap:{name}",
            ]
        )
        require(status == 0, "identity_generation_failed")
    _ = protected_file(target.identity)
    sync_file(target.identity)
    public_path = target.identity.with_suffix(target.identity.suffix + ".pub")
    if public_path.exists() or public_path.is_symlink():
        _ = protected_file(public_path, secret=False)
        sync_file(public_path)
    status, public = run_command(["ssh-keygen", "-y", "-P", "", "-f", str(target.identity)])
    require(
        status == 0 and re.fullmatch(r"ssh-ed25519 [A-Za-z0-9+/=]+(?: [^\r\n]+)?", public),
        "unsupported_identity",
    )
    return public


def enrollment_command(public: str) -> str:
    """Append this public key while retaining all existing authorized_keys contents."""
    script = (
        "set -eu; umask 077; "
        'test ! -L "$HOME/.ssh"; mkdir -p "$HOME/.ssh"; chmod 700 "$HOME/.ssh"; '
        'file="$HOME/.ssh/authorized_keys"; test ! -L "$file"; '
        'if test -e "$file"; then test -f "$file"; else : > "$file"; fi; '
        'chmod 600 "$file"; '
        f"key={shlex.quote(public)}; "
        'if ! grep -qxF "$key" "$file"; then printf "\\n%s\\n" "$key" >> "$file"; fi; '
        f"printf '%s\\n' {ENROLLED}"
    )
    return "sh -c " + shlex.quote(script)


def enroll(target: BootstrapTarget, public: str, args: BootstrapOptions) -> str:
    """Prefer existing key access; handle forced password rotation and one reconnect."""
    status, text = run_command(ssh_command(target, "printf '%s\\n' SIMPLESTCHAT_KEY_READY"))
    if status == 0 and "SIMPLESTCHAT_KEY_READY" in text.splitlines():
        return "existing_key"
    require(args.replacement_password_file is not None, "replacement_password_file_required")
    if args.replacement_password_file is None:
        raise BootstrapError("replacement_password_file_required")
    replacement = replacement_password(local_path(args.replacement_password_file), ROOT)
    initial = password_value(
        local_path(args.password_file) if args.password_file else None,
        "Initial SSH password: ",
        ROOT,
    )
    command = ssh_command(target, enrollment_command(public), batch=False, tty=True)
    first = password_session(command, initial, replacement)
    if not first.enrolled and first.rotated:
        # Debian commonly exits after PAM's forced password change, before the
        # requested command starts. Exactly one fresh login uses the new secret.
        second = password_session(command, replacement, replacement)
        require(second.enrolled and second.exit_status == 0, "key_enrollment_failed_after_rotation")
    else:
        require(first.enrolled and first.exit_status == 0, "key_enrollment_failed")
    status, text = run_command(ssh_command(target, "printf '%s\\n' SIMPLESTCHAT_KEY_READY"))
    require(
        status == 0 and "SIMPLESTCHAT_KEY_READY" in text.splitlines(),
        "enrolled_key_verification_failed",
    )
    return "password_rotated_and_key_enrolled" if first.rotated else "key_enrolled"


PREFLIGHT = r"""
import fcntl, json, os, pathlib, platform, shutil, subprocess
data = {}
for line in pathlib.Path('/etc/os-release').read_text().splitlines():
    key, separator, value = line.partition('=')
    if separator: data[key] = value.strip('"')
sudo = os.geteuid() == 0 or subprocess.run(['sudo', '-n', 'true'],
    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0
disk = os.statvfs('/srv' if pathlib.Path('/srv').exists() else '/')
facts = {'distribution':data.get('ID'), 'version':data.get('VERSION_ID'),
 'architecture':platform.machine(), 'cpus':os.cpu_count(),
 'freeBytes':disk.f_bavail * disk.f_frsize, 'freeInodes':disk.f_favail,
 'kernel':platform.release(), 'systemd':pathlib.Path('/run/systemd/system').is_dir(),
 'passwordlessSudo':sudo, 'python':platform.python_version(),
 'dockerInstalled':shutil.which('docker') is not None}
print('SIMPLESTCHAT_BOOTSTRAP_FACTS ' + json.dumps(facts))
"""


BUSY_CHECK = r"""
import fcntl, json, os, pathlib, stat, subprocess
root = pathlib.Path('/run/simplestchat-bench')
busy = False
if root.exists():
    info = root.lstat()
    assert stat.S_ISDIR(info.st_mode) and info.st_uid == 0 and stat.S_IMODE(info.st_mode) == 0o700
    lock = root / 'workload.lock'
    if lock.exists() or lock.is_symlink():
        fd = os.open(lock, os.O_RDONLY | os.O_NOFOLLOW)
        info = os.fstat(fd)
        assert stat.S_ISREG(info.st_mode) and info.st_uid == 0
        assert stat.S_IMODE(info.st_mode) == 0o600
        try: fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError: busy = True
        finally: os.close(fd)
    record = root / 'current.json'
    if record.exists() or record.is_symlink():
        info = record.lstat()
        assert stat.S_ISREG(info.st_mode) and info.st_uid == 0
        assert stat.S_IMODE(info.st_mode) == 0o600 and info.st_size <= 16384
        value = json.loads(record.read_text())
        busy = busy or value.get('schemaVersion') != 1 or value.get('finalized') is not True
for unit in ('simplestchat-image-build.service', 'simplestchat-benchmark.service'):
    result = subprocess.run(['systemctl', 'is-active', unit], capture_output=True, text=True)
    assert result.returncode in (0, 3, 4)
    busy = busy or result.stdout.strip() in ('active', 'activating', 'deactivating', 'reloading')
print('SIMPLESTCHAT_BOOTSTRAP_FACTS ' + json.dumps({'busy':busy}))
"""


def remote_python(target: BootstrapTarget, script: str, *, sudo: bool = False) -> JsonObject:
    """Read only explicitly allowlisted host metadata over the enrolled key."""
    command = "sudo -n " if sudo and target.user != "root" else ""
    command += "python3 -c " + shlex.quote(script)
    status, text = run_command(ssh_command(target, command), timeout=45)
    require(status == 0, "remote_preflight_failed")
    prefix = "SIMPLESTCHAT_BOOTSTRAP_FACTS "
    records = [line.removeprefix(prefix) for line in text.splitlines() if line.startswith(prefix)]
    require(len(records) == 1, "invalid_preflight_envelope")
    return object_value(decode_json(records[0]))


def preflight(target: BootstrapTarget, minimum_free_gib: int) -> JsonObject:
    """Require the supported platform and capacity before any host provisioning."""
    facts = remote_python(target, PREFLIGHT)
    require(
        facts.get("distribution") == "debian" and facts.get("version") == "13",
        "unsupported_distribution",
    )
    require(
        facts.get("architecture") == "x86_64" and facts.get("systemd") is True,
        "unsupported_host_platform",
    )
    free = facts.get("freeBytes")
    require(isinstance(free, int) and free >= minimum_free_gib * GIB, "insufficient_disk_space")
    if facts.get("passwordlessSudo") is True:
        require(
            remote_python(target, BUSY_CHECK, sudo=True).get("busy") is False,
            "host_workload_active_or_unfinished",
        )
    return facts


def inventory_ssh_common_args(target: BootstrapTarget) -> str:
    """Pin Ansible's SSH transport to the same endpoint/trust as direct preflight."""
    # Inventory's port/key have dedicated fields; all remaining SSH controls are
    # fixed here, rather than inheriting arbitrary controller host aliases.
    common = [
        "-F",
        "/dev/null",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        f"UserKnownHostsFile={target.known_hosts}",
        "-o",
        "GlobalKnownHostsFile=/dev/null",
        "-o",
        "IdentityAgent=none",
        "-o",
        "ForwardAgent=no",
        "-o",
        "IdentitiesOnly=yes",
        "-o",
        "PasswordAuthentication=no",
        "-o",
        "KbdInteractiveAuthentication=no",
        "-o",
        "BatchMode=yes",
        "-o",
        "ControlMaster=no",
        "-o",
        "ControlPath=none",
    ]
    return shlex.join(common)


def inventory_value(target: BootstrapTarget, args: BootstrapOptions) -> JsonObject:
    """Build one secret-free inventory whose OS-maintenance defaults remain off."""
    return {
        "benchmark_hosts": {
            "hosts": {
                args.name: {
                    "ansible_host": target.host,
                    "ansible_user": target.user,
                    "ansible_port": target.port,
                    "ansible_ssh_private_key_file": str(target.identity),
                    "ansible_ssh_common_args": inventory_ssh_common_args(target),
                    "scbench_ssh_known_hosts_file": str(target.known_hosts),
                    "scbench_revision": args.revision,
                    "scbench_upgrade_packages": False,
                    "scbench_reboot": False,
                }
            }
        }
    }


def string_object(value: object) -> dict[str, object]:
    """Validate a YAML object's keys before constructing a typed dictionary."""
    require(isinstance(value, dict), "invalid_inventory")
    if not isinstance(value, dict):
        raise BootstrapError("invalid_inventory")
    result: dict[str, object] = {}
    for key, item in cast("dict[object, object]", value).items():
        require(isinstance(key, str), "invalid_inventory_key")
        if isinstance(key, str):
            result[key] = item
    return result


def prepare_inventory(path: Path, target: BootstrapTarget, args: BootstrapOptions) -> None:
    """Create an ignored inventory, or preserve an existing matching one verbatim."""
    require(
        path.parent == ROOT / "ops/ansible" and path.name.startswith("inventory.local."),
        "inventory_must_use_ignored_local_name",
    )
    status, _ = run_command(["git", "check-ignore", "-q", "--", str(path)])
    require(status == 0, "inventory_not_ignored")
    if not path.exists():
        write_record(path, inventory_value(target, args))
        return
    data = cast("object", yaml.safe_load(protected_file(path)))
    inventory = string_object(data)
    require(set(inventory) == {"benchmark_hosts"}, "unsupported_inventory_groups")
    group = string_object(inventory.get("benchmark_hosts"))
    require(set(group) == {"hosts"}, "unsupported_inventory_group_variables")
    hosts = string_object(group.get("hosts"))
    require(set(hosts) == {args.name}, "inventory_must_select_one_host")
    host = string_object(hosts[args.name])
    require(
        host.get("ansible_host") == target.host and host.get("ansible_user") == target.user,
        "inventory_identity_mismatch",
    )
    require(host.get("ansible_port", 22) == target.port, "inventory_port_mismatch")
    identity = host.get("ansible_ssh_private_key_file")
    require(
        isinstance(identity, str) and local_path(identity) == target.identity,
        "inventory_key_mismatch",
    )
    require(
        host.get("scbench_upgrade_packages", False) is False
        and host.get("scbench_reboot", False) is False,
        "inventory_maintenance_flags_must_be_false",
    )
    known = host.get("scbench_ssh_known_hosts_file", str(Path.home() / ".ssh/known_hosts"))
    require(
        isinstance(known, str) and local_path(known) == target.known_hosts,
        "inventory_known_hosts_mismatch",
    )
    expected = string_object(
        string_object(string_object(inventory_value(target, args)["benchmark_hosts"])["hosts"])[
            args.name
        ]
    )["ansible_ssh_common_args"]
    require(
        host.get("ansible_ssh_common_args", "") in ("", expected), "unsupported_inventory_ssh_args"
    )
    require(host.get("ansible_connection", "ssh") == "ssh", "unsupported_inventory_connection")
    allowed_transport = {
        "ansible_host",
        "ansible_user",
        "ansible_port",
        "ansible_connection",
        "ansible_ssh_private_key_file",
        "ansible_ssh_common_args",
    }
    require(
        all(not key.startswith("ansible_") or key in allowed_transport for key in host),
        "unsupported_inventory_transport_override",
    )
    require(
        not any(
            key in host
            for key in (
                "ansible_password",
                "ansible_become_password",
                "ansible_ssh_pass",
                "ansible_ssh_executable",
            )
        ),
        "unsupported_inventory_authentication",
    )


def provision(
    target: BootstrapTarget,
    args: BootstrapOptions,
    inventory: Path,
    facts: JsonObject,
    output: Path,
) -> None:
    """Apply one-run revision/maintenance overrides, retaining no sudo credentials."""
    command = site_command(
        ROOT, inventory, args.name, args.revision, maintenance=args.initial_maintenance
    )
    command += [
        "--extra-vars",
        json.dumps(
            {
                "ansible_ssh_common_args": inventory_ssh_common_args(target),
            }
        ),
    ]
    evidence = output / "provision"
    evidence.mkdir(mode=0o700)
    with contextlib.ExitStack() as resources:
        if facts.get("passwordlessSudo") is not True:
            source = local_path(args.become_password_file) if args.become_password_file else None
            password = password_value(source, "Sudo password: ", ROOT)
            temporary = resources.enter_context(
                tempfile.TemporaryDirectory(
                    prefix=".simplestchat-become-", dir=target.identity.parent
                )
            )
            password_path = Path(temporary) / "password"
            write_new(password_path, (password + "\n").encode())
            command += ["--become-password-file", str(password_path)]
        try:
            status, _ = BUILD.Runner(output=evidence).run(
                command,
                cwd=ROOT,
                env=ansible_environment(ROOT),
                timeout=7200,
                allow_failure=True,
                capture=False,
            )
        except (BUILD.BuildError, OSError):
            raise BootstrapError("provisioning_interrupted_inspect_host") from None
        require(status == 0, "provisioning_failed_inspect_host")


def options(argv: list[str] | None = None) -> BootstrapOptions:
    """Validate public selectors before creating local files or contacting a host."""
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("host", "name", "revision", "identity", "inventory", "output"):
        _ = parser.add_argument("--" + flag, required=True)
    _ = parser.add_argument("--user", default="debian")
    _ = parser.add_argument("--port", type=int, default=22)
    _ = parser.add_argument("--known-hosts", default="~/.ssh/known_hosts")
    _ = parser.add_argument(
        "--host-fingerprint", help="Independently verified SHA256 Ed25519 fingerprint"
    )
    _ = parser.add_argument("--password-file", help="Owner-only file outside checkout, or prompt")
    _ = parser.add_argument(
        "--replacement-password-file", help="Saved before forced password change"
    )
    _ = parser.add_argument("--become-password-file", help="Optional protected sudo password file")
    _ = parser.add_argument("--minimum-free-gib", type=int, default=MINIMUM_FREE_GIB)
    _ = parser.add_argument("--provision", action="store_true")
    _ = parser.add_argument(
        "--initial-maintenance",
        action="store_true",
        help="One-run OS updates/reboot; requires --provision",
    )
    args = parser.parse_args(argv, namespace=BootstrapOptions())
    require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9.:-]{0,252}", args.host), "invalid_host")
    require(re.fullmatch(r"[a-z_][a-z0-9_-]{0,31}", args.user), "invalid_user")
    require(re.fullmatch(r"[A-Za-z][A-Za-z0-9_-]{0,62}", args.name), "invalid_inventory_name")
    require(re.fullmatch(r"[a-f0-9]{40}", args.revision), "invalid_revision")
    require(1 <= args.port <= MAX_PORT, "invalid_port")
    require(1 <= args.minimum_free_gib <= MAX_MINIMUM_DISK_GIB, "invalid_minimum_disk")
    require(not args.initial_maintenance or args.provision, "maintenance_requires_provision")
    require(
        args.host_fingerprint is None
        or re.fullmatch(r"SHA256:[A-Za-z0-9+/]{43}", args.host_fingerprint),
        "invalid_host_fingerprint",
    )
    return args


def bootstrap(args: BootstrapOptions) -> Path:
    """Perform explicitly selected access/provisioning stages with private evidence."""
    target = BootstrapTarget(
        args.host, args.user, args.port, local_path(args.identity), local_path(args.known_hosts)
    )
    inventory, output = local_path(args.inventory), local_path(args.output)
    require(not output.exists(), "evidence_directory_already_exists")
    status, revision = run_command(["git", "rev-parse", "HEAD"])
    require(status == 0 and revision == args.revision, "checkout_revision_mismatch")
    status, dirty = run_command(["git", "status", "--porcelain", "--untracked-files=all"])
    require(status == 0 and not dirty, "checkout_must_be_clean")
    if output.is_relative_to(ROOT):
        status, _ = run_command(["git", "check-ignore", "-q", "--", str(output / "plan.json")])
        require(status == 0, "evidence_directory_not_ignored")
    output.mkdir(mode=0o700, parents=False)
    write_record(
        output / "plan.json",
        {
            "schemaVersion": 1,
            "host": target.host,
            "user": target.user,
            "port": target.port,
            "name": args.name,
            "revision": args.revision,
            "inventory": str(inventory),
            "identity": str(target.identity),
            "knownHosts": str(target.known_hosts),
            "provision": args.provision,
            "initialMaintenance": args.initial_maintenance,
            "minimumFreeGiB": args.minimum_free_gib,
        },
    )
    try:
        if inventory.exists():
            # Validate local selectors before any remote authentication changes.
            prepare_inventory(inventory, target, args)
        trust = host_trust(target, args.host_fingerprint)
        public = ensure_identity(target, args.name)
        access = enroll(target, public, args)
        write_record(output / "access.json", {"mode": access, "hostTrust": trust})
        facts = preflight(target, args.minimum_free_gib)
        write_record(output / "host-before.json", facts)
        prepare_inventory(inventory, target, args)
        if args.provision:
            provision(target, args, inventory, facts, output)
            write_record(output / "host-after.json", preflight(target, args.minimum_free_gib))
        write_record(
            output / "outcome.json",
            {"completed": True, "provisioned": args.provision, "revision": args.revision},
        )
    except (BootstrapError, OSError, ValueError, yaml.YAMLError):
        write_record(output / "outcome.json", {"completed": False, "revision": args.revision})
        raise
    return output


def main(argv: list[str] | None = None) -> int:
    """Run the controller, exposing only fixed failures and the evidence location."""
    try:
        resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
        output = bootstrap(options(argv))
    except BootstrapError as error:
        print(f"bootstrap: {error}", file=sys.stderr)  # noqa: T201 -- Redacted CLI status.
        return 1
    except (OSError, ValueError, yaml.YAMLError):
        print("bootstrap: invalid_local_or_remote_metadata", file=sys.stderr)  # noqa: T201
        return 1
    print(f"Bootstrap evidence: {output}")  # noqa: T201 -- Explicit CLI result.
    print("Inventory maintenance flags remain false; use run-vps-capacity.py for build and tests.")  # noqa: T201
    return 0
