#!/usr/bin/env python3
"""Verify real deployment and local backup behavior inside one marked disposable VM.

The host installs this command and the canonical operations helpers, runs the
maintained playbooks, and destroys its VM in a finally block. This command accepts
no hostnames, database URLs, image selectors, SQL, or resource names. Root, the
Debian/QEMU identity, and a private run marker must agree before any command runs.
Only aggregate success evidence leaves the guest; command output stays private.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import signal
import stat
import subprocess
import sys
import tempfile
import time
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, NoReturn, Unpack, override

LIBRARY = Path("/usr/local/libexec/simplestchat-public")
sys.path.insert(0, str(LIBRARY))

import backup_public as backup  # noqa: E402 - guest helpers have a fixed installation path.
import release_public as release  # noqa: E402
import restore_verify as restore  # noqa: E402
from release_artifact import Manifest, validate_manifest  # noqa: E402
from release_json import (  # noqa: E402
    JsonObject,
    array_value,
    decode_json,
    object_value,
    string_value,
)

if TYPE_CHECKING:
    from collections.abc import Generator, Sequence
    from types import FrameType

ROOT = Path("/root/simplestchat-vm-fixture")
MARKER = Path("/etc/simplestchat-vm-fixture")
MAX_FILE = 1024 * 1024
MAX_MEMORY = 8 * 1024**3
MAX_PIDS = 1024
MAX_CPUS = 8
HTTP_PORT = 3000
SOCKET_MODE = 0o660
SNAPSHOT_PASSES = 2
PROJECT = "simplestchat-public"
LAUNCHER = "/usr/local/bin/simplestchat-public"
SERVICES = {
    "simplestchat": "10001:10001",
    "migrate": "10001:10001",
    "postgres": "999:999",
    "caddy": "10001:10001",
}
SECRET_NAMES = frozenset(
    {
        "postgres_admin_password",
        "migration_password",
        "database_password",
        "jwt_secret",
        "metrics_token",
        "proxy_secret",
        "owner_password",
    }
)
FILE_POLICIES = {
    "secrets.json": (0, 0o600),
    "compose.base.yml": (0, 0o644),
    "Caddyfile": (0, 0o644),
    "compose.public.yml": (0, 0o600),
    "app.env": (0, 0o600),
    "migration.env": (0, 0o600),
    "proxy.env": (0, 0o600),
    "postgres-admin-password": (999, 0o400),
    "pg_hba.conf": (0, 0o644),
    "init-database.sql": (999, 0o400),
    "runtime-grants.sql": (0, 0o600),
    "images.json": (0, 0o600),
}
COUNTS: JsonObject = {
    "users": 1,
    "rooms": 1,
    "sessions": 0,
    "credentials": 0,
    "activeIncidents": 1,
    "resolvedIncidents": 1,
}
FIXTURE_SQL = b"""BEGIN;
SET LOCAL statement_timeout='5s';
INSERT INTO public.users(id,email,display_name)
VALUES ('00000000-0000-0000-0000-000000000001','fixture@example.test','Fixture');
INSERT INTO public.rooms(id,owner_id,display_name)
VALUES ('fixture','00000000-0000-0000-0000-000000000001','Fixture');
INSERT INTO operations.alerts(incident_key,rule,severity,resource,first_seen,last_seen,resolved_at)
VALUES (repeat('a',64),'DatabaseUnavailable','critical','{}',now(),now(),NULL),
       (repeat('b',64),'CollectorStale','warning','{}',now(),now(),now());
INSERT INTO operations.alert_cursor VALUES (true,now());
COMMIT;
"""
INSPECTION = (
    '{"id":{{json .Id}},"name":{{json .Name}},"image":{{json .Image}},'
    + '"user":{{json .Config.User}},"labels":{{json .Config.Labels}},'
    + '"host":{{json .HostConfig}},"state":{{json .State}}}'
)


@dataclass(frozen=True, slots=True, kw_only=True)
class Selection:
    """Bind every assertion and resource to the host's one immutable fixture run."""

    run_id: str
    revision: str


class Arguments(argparse.Namespace):
    """Accept only three fixed guest actions and the exact external ownership token."""

    action: str = ""
    run_id: str = ""


class GuestRunner(release.Runner):
    """Retain canonical process/output bounds and enforce a shared polling deadline."""

    def __init__(self, attempt: Path) -> None:
        """Start without a phase deadline; every command still has the canonical timeout."""
        super().__init__(attempt)
        self.deadline: float | None = None

    @override
    def run(self, args: Sequence[str], **options: Unpack[release.CommandOptions]) -> bytes:
        """Never let another inspection command extend a phase's absolute deadline."""
        if self.deadline is not None:
            remaining = self.deadline - time.monotonic()
            release.require(remaining > 0, "Fixture phase deadline exceeded")
            options["timeout"] = min(options.get("timeout", 30), remaining)
        return super().run(args, **options)


def private_file(path: Path, *, uid: int = 0, gid: int | None = None, mode: int = 0o600) -> bytes:
    """Read one bounded ordinary file without accepting links or unexpected ownership."""
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as source:
        before = os.fstat(source.fileno())
        release.require(
            stat.S_ISREG(before.st_mode)
            and before.st_uid == uid
            and before.st_gid == (uid if gid is None else gid)
            and stat.S_IMODE(before.st_mode) == mode
            and before.st_nlink == 1
            and 0 < before.st_size <= MAX_FILE,
            "Fixture file ownership, type, size or mode differs",
        )
        data = source.read(MAX_FILE + 1)
        after = os.fstat(source.fileno())
    release.require(
        len(data) == before.st_size
        and (before.st_size, before.st_mtime_ns, before.st_ctime_ns)
        == (after.st_size, after.st_mtime_ns, after.st_ctime_ns),
        "Fixture file changed while reading",
    )
    return data


def directory(path: Path, *, uid: int = 0, mode: int = 0o700) -> JsonObject:
    """Require exact directory ownership without following its final path component."""
    metadata = path.lstat()
    release.require(
        stat.S_ISDIR(metadata.st_mode)
        and metadata.st_uid == uid
        and metadata.st_gid == uid
        and stat.S_IMODE(metadata.st_mode) == mode,
        "Fixture directory ownership or mode differs",
    )
    return {"uid": uid, "gid": uid, "mode": mode}


def selection(data: bytes, run_id: str) -> Selection:
    """Reject ambiguous schema versions, added options, and mismatched run identities."""
    value = object_value(decode_json(data))
    release.require(
        re.fullmatch(r"[a-f0-9]{32}", run_id)
        and set(value) == {"schemaVersion", "runId", "revision"}
        and type(value["schemaVersion"]) is int
        and value["schemaVersion"] == 1
        and value["runId"] == run_id,
        "Fixture selection schema or owner differs",
    )
    revision = string_value(value["revision"])
    release.require(re.fullmatch(r"[a-f0-9]{40}", revision), "Fixture revision is invalid")
    return Selection(run_id=run_id, revision=revision)


def guard(run_id: str) -> Selection:
    """Reject an ordinary host before creating evidence or contacting the Docker daemon."""
    release.require(os.geteuid() == 0, "Fixture requires root")
    release.require(
        platform.system() == "Linux" and platform.machine() == "x86_64",
        "Fixture requires Linux x86_64",
    )
    release.require(re.fullmatch(r"[a-f0-9]{32}", run_id), "Invalid fixture ownership token")
    system = platform.freedesktop_os_release()
    release.require(
        system.get("ID") == "debian" and system.get("VERSION_ID") == "13",
        "Fixture requires Debian 13",
    )
    with Path("/sys/class/dmi/id/sys_vendor").open("rb") as source:
        release.require(source.read(256).strip() == b"QEMU", "Fixture requires QEMU")
    _ = directory(ROOT)
    release.require(private_file(MARKER) == (run_id + "\n").encode(), "Fixture marker differs")
    chosen = selection(private_file(ROOT / "selection.json"), run_id)
    _ = directory(LIBRARY, mode=0o755)
    for name in (
        "bounded_process",
        "release_public",
        "release_artifact",
        "release_json",
        "backup_public",
        "restore_verify",
    ):
        module = sys.modules[name]
        release.require(
            module.__file__ == str(LIBRARY / (name + ".py")), "Fixture helper origin differs"
        )
        _ = private_file(LIBRARY / (name + ".py"), mode=0o644)
    socket = Path("/run/docker.sock").lstat()
    release.require(
        stat.S_ISSOCK(socket.st_mode)
        and socket.st_uid == 0
        and stat.S_IMODE(socket.st_mode) == SOCKET_MODE,
        "Fixture Docker socket differs",
    )
    return chosen


def properties(runner: release.RunnerProtocol, unit: str, names: tuple[str, ...]) -> dict[str, str]:
    """Read an exact bounded set of effective systemd properties without parsing status logs."""
    output = runner.run(
        ["/usr/bin/systemctl", "show", unit, "--no-pager", "--property=" + ",".join(names)]
    )
    result: dict[str, str] = {}
    for line in output.decode().splitlines():
        key, separator, value = line.partition("=")
        release.require(
            separator and key in names and key not in result, "Unexpected systemd property"
        )
        result[key] = value
    release.require(set(result) == set(names), "Incomplete systemd properties")
    return result


def validate_unit(value: dict[str, str], unit: str, *, backup_service: bool = False) -> None:
    """Require root execution, private runtime state, and the selected fixed unit file."""
    expected = {
        "LoadState": "loaded",
        "FragmentPath": "/etc/systemd/system/" + unit,
        "UMask": "0077",
        "DynamicUser": "no",
    }
    release.require(
        all(value.get(key) == item for key, item in expected.items())
        and value.get("User") in ("", "root", "0"),
        "Fixture unit identity differs",
    )
    if not backup_service:
        release.require(
            value.get("ActiveState") == "inactive"
            and value.get("UnitFileState") in ("static", "disabled"),
            "Fixture workload unit is enabled or active",
        )
    if unit != "simplestchat-image-build.service":
        release.require(
            value.get("RuntimeDirectory") == "simplestchat-bench"
            and value.get("RuntimeDirectoryMode") == "0700"
            and value.get("RuntimeDirectoryPreserve") == "yes",
            "Fixture runtime directory differs",
        )
    else:
        release.require(
            value.get("RuntimeDirectory") == "", "Unexpected image build runtime directory"
        )


UNIT_PROPERTIES = (
    "LoadState",
    "FragmentPath",
    "User",
    "DynamicUser",
    "UMask",
    "RuntimeDirectory",
    "RuntimeDirectoryMode",
    "RuntimeDirectoryPreserve",
    "ActiveState",
    "UnitFileState",
)


def configured_service(value: JsonObject, name: str) -> None:
    """Check effective Compose isolation, nonroot users, and finite workload budgets."""
    release.require(
        name in SERVICES and value.get("user") == SERVICES[name], "Fixture Compose user differs"
    )
    release.require(
        value.get("read_only") is True
        and value.get("cap_drop") == ["ALL"]
        and value.get("privileged", False) is False,
        "Fixture Compose isolation differs",
    )
    release.require(
        value.get("cap_add", []) == (["NET_BIND_SERVICE"] if name == "caddy" else []),
        "Fixture Compose added capabilities differ",
    )
    release.require(
        value.get("security_opt") == ["no-new-privileges:true"],
        "Fixture Compose privilege boundary differs",
    )
    memory, pids = value.get("mem_limit"), value.get("pids_limit")
    cpus = value.get("cpus")
    release.require(
        type(memory) is int
        and 0 < memory <= MAX_MEMORY
        and type(pids) is int
        and 0 < pids <= MAX_PIDS,
        "Fixture Compose resource limits differ",
    )
    release.require(
        type(cpus) in (int, float, str)
        and 0 < float(string_value(cpus) if isinstance(cpus, str) else str(cpus)) <= MAX_CPUS,
        "Fixture Compose CPU limit differs",
    )
    if name in ("postgres", "migrate"):
        release.require(value.get("network_mode") == "none", "Fixture isolated network differs")
    if name != "postgres":
        release.require(value.get("init") is True, "Fixture Compose init differs")
    if name == "simplestchat":
        ports = [object_value(port) for port in array_value(value["ports"])]
        http = [port for port in ports if port.get("protocol") == "tcp"]
        release.require(
            len(http) == 1
            and http[0].get("target") == HTTP_PORT
            and http[0].get("published") == "3000"
            and http[0].get("host_ip") == "127.0.0.1",
            "Fixture HTTP publication is not loopback-only",
        )


def configuration(runner: release.RunnerProtocol) -> JsonObject:
    """Validate the actual rendered Compose model while retaining only its digest."""
    document = object_value(
        decode_json(
            runner.run([LAUNCHER, "--profile", "maintenance", "config", "--format", "json"])
        )
    )
    release.require(document.get("name") == PROJECT, "Fixture Compose project differs")
    services = object_value(document["services"])
    release.require(set(services) == set(SERVICES), "Fixture Compose services differ")
    for name, service in services.items():
        configured_service(object_value(service), name)
    return {
        "sha256": hashlib.sha256(
            json.dumps(document, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest(),
        "services": len(services),
    }


def public_files() -> JsonObject:
    """Validate every public configuration path and store hashes instead of secret bytes."""
    result: JsonObject = {}
    for path, uid, mode in (
        (release.CONFIG, 0, 0o700),
        (release.ROOT, 0, 0o700),
        (release.ROOT / "results", 0, 0o700),
        (release.ROOT / "backups", 0, 0o700),
        (release.ROOT / "postgres", 999, 0o700),
        (release.ROOT / "postgres-socket", 999, 0o755),
        (release.ROOT / "caddy-data", 10001, 0o700),
        (release.ROOT / "caddy-config", 10001, 0o700),
    ):
        result[str(path)] = directory(path, uid=uid, mode=mode)
    release.require(
        {path.name for path in release.CONFIG.iterdir()} == set(FILE_POLICIES),
        "Unexpected public configuration files",
    )
    for name, (uid, mode) in FILE_POLICIES.items():
        data = private_file(release.CONFIG / name, uid=uid, mode=mode)
        result[str(release.CONFIG / name)] = {
            "uid": uid,
            "gid": uid,
            "mode": mode,
            "bytes": len(data),
            "sha256": hashlib.sha256(data).hexdigest(),
        }
    secrets = object_value(decode_json(private_file(release.CONFIG / "secrets.json")))
    release.require(
        frozenset(secrets) == SECRET_NAMES
        and all(
            isinstance(value, str) and re.fullmatch(r"[a-f0-9]{64}", value)
            for value in secrets.values()
        ),
        "Fixture deployment secret schema differs",
    )
    release.require(
        len({string_value(value) for value in secrets.values()}) == len(SECRET_NAMES),
        "Fixture secrets are not independent",
    )
    environment = release.dotenv_lines(private_file(release.CONFIG / "app.env").decode())
    expected = {
        "ALLOWED_ORIGINS": "https://vm-fixture.test",
        "WEBAUTHN_RP_ID": "vm-fixture.test",
        "WEBAUTHN_ORIGIN": "https://vm-fixture.test",
        "ANNOUNCE_IP": "127.0.0.1",
        "ANNOUNCE_IPV6": "",
        "RUN_MIGRATIONS": "false",
        "REGISTRATION_ENABLED": "false",
        "ALLOW_AD_HOC_ROOMS": "false",
        "MEDIA_WORKERS": "1",
        "BIND_ADDR": "0.0.0.0",  # noqa: S104 - container namespace; host HTTP publication is loopback.
        "PORT": "3000",
        "JWT_SECRET": string_value(secrets["jwt_secret"]),
        "METRICS_TOKEN": string_value(secrets["metrics_token"]),
        "TRUSTED_PROXY_SECRET": string_value(secrets["proxy_secret"]),
        "DATABASE_URL": "postgres://simplestchat_app:"
        + string_value(secrets["database_password"])
        + "@localhost/simplestchat?host=/run/simplestchat-postgres&sslmode=disable",
    }
    release.require(
        all(environment.get(key) == value for key, value in expected.items()),
        "Fixture application configuration differs",
    )
    migration = release.dotenv_lines(private_file(release.CONFIG / "migration.env").decode())
    release.require(
        migration
        == {
            "DATABASE_URL": "postgres://simplestchat_migrate:"
            + string_value(secrets["migration_password"])
            + "@localhost/simplestchat?host=/run/simplestchat-postgres&sslmode=disable",
            "RUN_MIGRATIONS": "true",
            "BIND_ADDR": "127.0.0.1",
            "PORT": "3000",
            "ANNOUNCE_IP": "127.0.0.1",
            "MEDIA_WORKERS": "1",
            "REGISTRATION_ENABLED": "false",
            "ALLOW_AD_HOC_ROOMS": "false",
            "RUST_LOG": "simplestChat=info,mediasoup=warn",
        },
        "Fixture migration environment differs",
    )
    proxy = release.dotenv_lines(private_file(release.CONFIG / "proxy.env").decode())
    release.require(
        proxy
        == {
            "CADDY_DOMAIN": "vm-fixture.test",
            "CADDY_UPSTREAM": "simplestchat:3000",
            "TRUSTED_PROXY_SECRET": string_value(secrets["proxy_secret"]),
        },
        "Fixture proxy environment differs",
    )
    release.require(
        private_file(release.CONFIG / "postgres-admin-password", uid=999, mode=0o400)
        .decode()
        .strip()
        == secrets["postgres_admin_password"],
        "Fixture database secret differs",
    )
    return result


def snapshot(runner: release.RunnerProtocol, chosen: Selection) -> JsonObject:
    """Compare two actual playbook applications without persisting resolved credentials."""
    files = public_files()
    units: JsonObject = {}
    for name in ("benchmark", "image-build"):
        unit = "simplestchat-" + name + ".service"
        value = properties(runner, unit, UNIT_PROPERTIES)
        validate_unit(value, unit)
        units[unit] = dict(value)
        content = private_file(Path("/etc/systemd/system") / unit, mode=0o644)
        files[unit] = {
            "sha256": hashlib.sha256(content).hexdigest(),
            "uid": 0,
            "gid": 0,
            "mode": 0o644,
        }
    _ = private_file(Path(LAUNCHER), mode=0o755)
    release.require(
        not runner.compose("ps", "--all", "--quiet").strip(),
        "Fixture public project is already present",
    )
    record: JsonObject = {
        "schemaVersion": 1,
        "runId": chosen.run_id,
        "files": files,
        "units": units,
        "compose": configuration(runner),
    }
    path = ROOT / "snapshot.json"
    if path.exists() or path.is_symlink():
        previous = object_value(decode_json(private_file(path)))
        prior_passes = previous.pop("passes", None)
        release.require(
            type(prior_passes) is int
            and prior_passes == 1
            and type(previous.get("schemaVersion")) is int
            and previous == record,
            "Public playbook changed protected configuration",
        )
        passes = SNAPSHOT_PASSES
    else:
        passes = 1
    release.atomic(path, {**record, "passes": passes})
    return {"configurationFiles": len(FILE_POLICIES), "idempotent": passes == SNAPSHOT_PASSES}


def artifact(runner: release.RunnerProtocol, chosen: Selection) -> tuple[Manifest, str, str]:
    """Bind the configuration, imported artifact and runtime images to one revision."""
    path = release.ROOT / "releases" / chosen.revision
    _ = directory(path)
    _ = private_file(path / "release.json")
    manifest = validate_manifest(path / "release.json")
    images = object_value(decode_json(private_file(release.CONFIG / "images.json")))
    staged = object_value(decode_json(private_file(path / "staged.json")))
    release.require(
        manifest["revision"] == chosen.revision
        and images.get("revision") == chosen.revision
        and staged.get("revision") == chosen.revision
        and staged.get("manifestSha256")
        == hashlib.sha256(private_file(path / "release.json")).hexdigest(),
        "Fixture release selection differs",
    )
    image = string_value(images["serverImage"])
    release.require(
        staged.get("serverImage") == image
        and release.image_identity(runner, image, chosen.revision) == image,
        "Fixture server image differs",
    )
    postgres = string_value(images["postgresImage"])
    release.require(
        re.fullmatch(r"docker.io/library/postgres:[a-z0-9.-]+@sha256:[a-f0-9]{64}", postgres),
        "Fixture PostgreSQL image is not immutable",
    )
    database_image = (
        runner.docker("image", "inspect", "--format", "{{.Id}}", postgres).decode().strip()
    )
    release.require(release.ID.fullmatch(database_image), "Fixture PostgreSQL image is missing")
    return manifest, image, database_image


def inspect_service(runner: release.RunnerProtocol, service: str) -> JsonObject:
    """Resolve exactly one public service, then verify its full ID and fixed name."""
    release.require(service in SERVICES, "Unknown fixture service")
    identifier = runner.compose("ps", "--all", "--quiet", service).decode().strip()
    release.require(
        re.fullmatch(r"[a-f0-9]{64}", identifier), "Fixture service identity is ambiguous"
    )
    value = object_value(decode_json(runner.docker("inspect", "--format", INSPECTION, identifier)))
    labels = object_value(value["labels"])
    release.require(
        value.get("id") == identifier
        and value.get("name") == f"/{PROJECT}-{service}-1"
        and labels.get("com.docker.compose.project") == PROJECT
        and labels.get("com.docker.compose.service") == service,
        "Fixture container ownership differs",
    )
    return value


def running_service(value: JsonObject, service: str, image: str) -> str:
    """Check the actual container, not just its declared Compose configuration."""
    state, host = object_value(value["state"]), object_value(value["host"])
    release.require(
        value.get("image") == image and value.get("user") == SERVICES[service],
        "Fixture runtime identity differs",
    )
    release.require(
        state.get("Running") is True and state.get("OOMKilled") is False,
        "Fixture service is not running normally",
    )
    release.require(
        host.get("ReadonlyRootfs") is True
        and host.get("Privileged") is False
        and host.get("CapDrop") == ["ALL"]
        and host.get("CapAdd") in (None, [])
        and host.get("SecurityOpt") in (["no-new-privileges"], ["no-new-privileges:true"]),
        "Fixture runtime isolation differs",
    )
    memory, pids, cpus = host.get("Memory"), host.get("PidsLimit"), host.get("NanoCpus")
    release.require(
        type(memory) is int
        and 0 < memory <= MAX_MEMORY
        and type(pids) is int
        and 0 < pids <= MAX_PIDS
        and type(cpus) is int
        and 0 < cpus <= MAX_CPUS * 10**9,
        "Fixture runtime limits differ",
    )
    if service in ("postgres", "migrate"):
        release.require(host.get("NetworkMode") == "none", "Fixture runtime network differs")
    return string_value(value["id"])


def remove_migrate(runner: release.RunnerProtocol, image: str, identifier: str) -> None:
    """Stop and remove only the exact maintenance container created by this action."""
    value = inspect_service(runner, "migrate")
    release.require(
        value.get("id") == identifier and value.get("image") == image,
        "Migration container identity changed",
    )
    _ = runner.docker("stop", "--time", "10", identifier, timeout=20)
    _ = runner.docker("rm", identifier, timeout=15)
    release.require(
        not runner.compose("ps", "--all", "--quiet", "migrate").strip(),
        "Migration container cleanup failed",
    )


def sql(runner: release.RunnerProtocol, database: str, source: bytes) -> bytes:
    """Run fixed fixture/schema SQL as the canonical peer-authenticated database owner."""
    return runner.docker(
        "exec",
        "--interactive",
        "--user",
        "999:999",
        database,
        "timeout",
        "--signal=TERM",
        "--kill-after=2s",
        "30s",
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
        "--quiet",
        input_data=source,
        timeout=35,
    )


def migrate(runner: GuestRunner, database: str, image: str, expected: dict[str, str]) -> None:
    """Start the real application migrator and require the complete packaged ledger."""
    _ = runner.run(
        [
            LAUNCHER,
            "--profile",
            "maintenance",
            "create",
            "--no-build",
            "--pull",
            "never",
            "migrate",
        ],
        timeout=60,
    )
    value = inspect_service(runner, "migrate")
    identifier = string_value(value["id"])
    release.require(value.get("image") == image, "Unexpected migration image")
    try:
        _ = runner.docker("start", identifier)
        deadline = time.monotonic() + 120
        runner.deadline = deadline
        while True:
            try:
                _ = running_service(inspect_service(runner, "migrate"), "migrate", image)
                actual = release.ledger(runner, database)
            except (release.ReleaseError, subprocess.TimeoutExpired):
                actual = {}
            if actual == expected:
                break
            release.require(time.monotonic() < deadline, "Packaged migration ledger was not ready")
            time.sleep(0.5)
    finally:
        runner.deadline = None
        remove_migrate(runner, image, identifier)


def database(runner: GuestRunner, chosen: Selection) -> JsonObject:
    """Create one synthetic database through real maintenance and application startup."""
    record = object_value(decode_json(private_file(ROOT / "snapshot.json")))
    release.require(
        record.get("runId") == chosen.run_id and record.get("passes") == SNAPSHOT_PASSES,
        "Public idempotence must pass first",
    )
    release.require(not (ROOT / "database.json").exists(), "Fixture database was already created")
    with release.workload_lock():
        manifest, image, database_image = artifact(runner, chosen)
        release.require(
            not runner.compose("ps", "--all", "--quiet").strip(),
            "Fixture public project must start empty",
        )
        _ = runner.run(
            [
                LAUNCHER,
                "up",
                "--detach",
                "--no-deps",
                "--no-build",
                "--pull",
                "never",
                "--wait",
                "--wait-timeout",
                "120",
                "postgres",
            ],
            timeout=130,
        )
        identifier = running_service(
            inspect_service(runner, "postgres"), "postgres", database_image
        )
        migrate(runner, identifier, image, manifest["migrations"])
        _ = sql(runner, identifier, private_file(release.CONFIG / "runtime-grants.sql"))
        _ = sql(runner, identifier, private_file(LIBRARY / "monitoring-schema.sql", mode=0o644))
        _ = sql(runner, identifier, FIXTURE_SQL)
        release.require(
            object_value(
                decode_json(
                    sql(
                        runner, identifier, private_file(LIBRARY / "restore-verify.sql", mode=0o644)
                    )
                )
            )
            == COUNTS,
            "Fixture source counts or privacy differ",
        )
        _ = runner.run(
            [
                LAUNCHER,
                "up",
                "--detach",
                "--no-deps",
                "--no-build",
                "--pull",
                "never",
                "simplestchat",
            ],
            timeout=60,
        )
        release.ready(runner, seconds=60)
        app = running_service(inspect_service(runner, "simplestchat"), "simplestchat", image)
        release.require(
            not runner.compose("ps", "--all", "--quiet", "caddy").strip(),
            "Fixture proxy must remain absent",
        )
        release.atomic(
            ROOT / "database.json",
            {
                "schemaVersion": 1,
                "runId": chosen.run_id,
                "postgres": identifier,
                "application": app,
                "postgresImage": database_image,
                "serverImage": image,
            },
        )
    return {"migrations": len(manifest["migrations"]), "ready": True, "counts": dict(COUNTS)}


def backup_units(runner: release.RunnerProtocol) -> None:
    """Verify the real timer and effective root service sandbox before starting a backup."""
    _ = directory(backup.TARGET)
    _ = private_file(LIBRARY / "backup-nightly.sh", mode=0o700)
    for name in ("service", "timer"):
        _ = private_file(Path("/etc/systemd/system/simplestchat-backup." + name), mode=0o644)
    expected = {
        "NoNewPrivileges": "yes",
        "PrivateTmp": "yes",
        "ProtectSystem": "strict",
        "ProtectHome": "yes",
        "RestrictSUIDSGID": "yes",
        "LockPersonality": "yes",
        "MemoryMax": "268435456",
        "TasksMax": "64",
        "CPUQuotaPerSecUSec": "500ms",
        "TimeoutStartUSec": "5min",
        "ReadWritePaths": "/srv/simplestchat-public/backups /run/simplestchat-bench",
    }
    value = properties(runner, "simplestchat-backup.service", (*UNIT_PROPERTIES, *expected))
    validate_unit(value, "simplestchat-backup.service", backup_service=True)
    release.require(
        all(value.get(key) == item for key, item in expected.items() if key != "ReadWritePaths")
        and set(value["ReadWritePaths"].split()) == set(expected["ReadWritePaths"].split()),
        "Backup service sandbox differs",
    )
    timer_expected = {
        "LoadState": "loaded",
        "UnitFileState": "enabled",
        "ActiveState": "active",
        "Persistent": "yes",
        "RandomizedDelayUSec": "10min",
        "AccuracyUSec": "1min",
        "Triggers": "simplestchat-backup.service",
    }
    timer = properties(runner, "simplestchat-backup.timer", tuple(timer_expected))
    release.require(timer == timer_expected, "Backup timer configuration differs")


@contextmanager
def paused_backup_timer(runner: GuestRunner) -> Generator[None]:
    """Serialize the explicitly started fixture backup with its real installed timer."""
    _ = runner.run(["/usr/bin/systemctl", "stop", "simplestchat-backup.timer"])
    try:
        runner.deadline = time.monotonic() + 310
        while True:
            state = properties(runner, "simplestchat-backup.service", ("ActiveState",))
            if state["ActiveState"] == "inactive":
                break
            release.require(
                state["ActiveState"] in ("active", "activating", "deactivating"),
                "A prior fixture backup failed",
            )
            time.sleep(0.5)
        runner.deadline = None
        # The canonical backup names archives at UTC second precision. Avoid a
        # same-second collision if timer installation just completed a backup.
        deadline = time.monotonic() + 3
        while (backup.TARGET / (time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + ".logs")).exists():
            release.require(time.monotonic() < deadline, "Fixture backup timestamp is occupied")
            time.sleep(0.1)
        yield
    finally:
        runner.deadline = None
        _ = runner.run(["/usr/bin/systemctl", "start", "simplestchat-backup.timer"])


def verify_backup(
    runner: release.RunnerProtocol,
    receipt_path: Path,
    manifest: Manifest,
    database_id: str,
    database_image: str,
) -> None:
    """Bind a completed archive to the fixture and prove isolated restore plus cleanup."""
    with release.workload_lock():
        archive, receipt, _ = release.validated_backup(receipt_path)
        release.require(
            running_service(inspect_service(runner, "postgres"), "postgres", database_image)
            == database_id
            and receipt.get("postgresImage") == database_image
            and restore.backup_migrations(receipt, manifest["migrations"]) == manifest["migrations"]
            and release.ledger(runner, database_id) == manifest["migrations"],
            "Backup identity or complete ledger differs",
        )
        attempt = Path(tempfile.mkdtemp(prefix="restore.", dir=ROOT))
        restore_runner = release.Runner(attempt)
        snapshot_path = attempt / "snapshot.dump"
        try:
            restore.snapshot(archive, snapshot_path, string_value(receipt["sha256"]))
            report = restore.verify(
                restore_runner, database_image, snapshot_path, manifest["migrations"]
            )
        finally:
            snapshot_path.unlink(missing_ok=True)
        release.require(
            report.get("verified") is True
            and report.get("cleanupPassed") is True
            and report.get("counts") == COUNTS,
            "Fixture restore verification or cleanup differs",
        )
        release.require(
            not runner.docker(
                "ps", "--all", "--quiet", "--filter", "label=" + restore.LABEL
            ).strip(),
            "Fixture restore container remains",
        )


def backup_restore(runner: GuestRunner, chosen: Selection) -> JsonObject:
    """Validate a new systemd backup and restore it in the canonical isolated verifier."""
    backup_units(runner)
    _ = public_files()
    state = object_value(decode_json(private_file(ROOT / "database.json")))
    release.require(state.get("runId") == chosen.run_id, "Fixture database ownership differs")
    manifest, image, database_image = artifact(runner, chosen)
    database_id = running_service(inspect_service(runner, "postgres"), "postgres", database_image)
    release.require(
        database_id == state.get("postgres")
        and running_service(inspect_service(runner, "simplestchat"), "simplestchat", image)
        == state.get("application"),
        "Fixture container changed before backup",
    )
    with paused_backup_timer(runner):
        before = set(backup.TARGET.glob("*.receipt.json"))
        _ = runner.run(["/usr/bin/systemctl", "start", "simplestchat-backup.service"], timeout=310)
        service = properties(
            runner, "simplestchat-backup.service", ("Result", "ExecMainStatus", "ActiveState")
        )
        release.require(
            service == {"Result": "success", "ExecMainStatus": "0", "ActiveState": "inactive"},
            "Backup service did not complete",
        )
        created = set(backup.TARGET.glob("*.receipt.json")) - before
        release.require(len(created) == 1, "Backup service did not publish exactly one new receipt")
        verify_backup(runner, next(iter(created)), manifest, database_id, database_image)
    backup_units(runner)
    release.ready(runner)
    return {
        "archiveValidated": True,
        "restoreVerified": True,
        "cleanupPassed": True,
        "counts": dict(COUNTS),
    }


def main() -> None:
    """Keep successful public evidence small and failures free of private command output."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("action", choices=("snapshot", "database", "backup-restore"))
    _ = parser.add_argument("--run-id", required=True)
    arguments = parser.parse_args(namespace=Arguments())
    chosen = guard(arguments.run_id)
    _ = os.umask(0o077)

    def interrupted(_signum: int, _frame: FrameType | None) -> NoReturn:
        message = "Fixture interrupted"
        raise release.ReleaseError(message)

    for signum in (signal.SIGINT, signal.SIGTERM):
        _ = signal.signal(signum, interrupted)
    attempt = Path(tempfile.mkdtemp(prefix=arguments.action + ".", dir=ROOT))
    runner = GuestRunner(attempt)
    actions = {"snapshot": snapshot, "database": database, "backup-restore": backup_restore}
    result = actions[arguments.action](runner, chosen)
    report: JsonObject = {
        "schemaVersion": 1,
        "runId": chosen.run_id,
        "action": arguments.action,
        "passed": True,
        **result,
    }
    release.atomic(attempt / "outcome.json", report)
    _ = sys.stdout.write(json.dumps(report, sort_keys=True) + "\n")


def cli() -> int:
    """Return a fixed failure category; private evidence remains inside the disposable guest."""
    try:
        main()
    except (OSError, ValueError, KeyError, release.ReleaseError, subprocess.SubprocessError):
        _ = sys.stderr.write("Disposable VM assertion failed; inspect private guest evidence.\n")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(cli())
