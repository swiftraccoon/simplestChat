"""Move one owned public deployment through a durable, explicit host protocol.

The controller transfers private artifacts and starts the destination only after
seal-source. Once sealed, source recovery is permanently refused. No action
builds images, deletes databases, edits DNS, or posts public chat messages.
"""

from __future__ import annotations

import argparse
import fcntl
import hmac
import json
import os
import re
import signal
import stat
import sys
import tempfile
from contextlib import contextmanager
from copy import deepcopy
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, NoReturn

import backup_public
import bounded_process
import migration_snapshot
import release_public as public
from release_artifact import sha256_file
from release_json import (
    JsonObject,
    JsonValue,
    array_value,
    decode_json,
    integer_value,
    object_value,
    string_value,
)

if TYPE_CHECKING:
    from collections.abc import Callable, Generator
    from types import FrameType

ACTIONS = (
    "inspect-source",
    "inspect-target",
    "freeze-source",
    "restore-target",
    "verify-target",
    "recover-source",
    "seal-source",
    "finalize-source",
    "abort-target",
)
POSTGRES_UID = 999
POSTGRES_DIRECTORY_MODE = 0o700
MAX_PRIVATE = 4 * 1024 * 1024
MAX_ORIGIN_BYTES = 261
MAX_DUMP = 512 * 1024 * 1024
DB_UNITS = (
    "simplestchat-monitoring.timer",
    "simplestchat-monitoring-external.timer",
    "simplestchat-backup.timer",
    "simplestchat-backup-upload.timer",
    "simplestchat-backup-restore.timer",
    "simplestchat-monitoring.service",
    "simplestchat-monitoring-external.service",
    "simplestchat-backup.service",
    "simplestchat-backup-upload.service",
    "simplestchat-backup-restore.service",
)
RETIRE_UNITS = (
    *DB_UNITS,
    "simplestchat-turn-certificate.timer",
    "simplestchat-turn-certificate.service",
)
CONTAINER_FORMAT = (
    '{"id":{{json .Id}},"name":{{json .Name}},"image":{{json .Image}},'
    '"running":{{json .State.Running}},"oom":{{json .State.OOMKilled}},'
    '"project":{{json (index .Config.Labels "com.docker.compose.project")}},'
    '"service":{{json (index .Config.Labels "com.docker.compose.service")}},'
    '"configHash":{{json (index .Config.Labels "com.docker.compose.config-hash")}}}'
)
IDENTITY_SQL = """SELECT jsonb_build_object(
 'systemIdentifier',(pg_control_system()).system_identifier::text,
 'serverVersion',current_setting('server_version_num'),
 'emptyDatabase',NOT EXISTS(SELECT FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
   WHERE n.nspname !~ '^pg_' AND n.nspname<>'information_schema' AND c.relkind IN ('r','p','S')),
 'otherDatabases',EXISTS(SELECT FROM pg_database
   WHERE NOT datistemplate AND datname NOT IN ('postgres','simplestchat')),
 'largeObjects',(SELECT count(*) FROM pg_largeobject_metadata));"""


@dataclass(frozen=True, slots=True, kw_only=True)
class Request:
    """Bind both hosts and one exact release to an immutable operation."""

    operation_id: str
    source_origin: str
    destination_origin: str
    target_revision: str

    def json(self) -> JsonObject:
        """Use the same four-field wire format on controller and both hosts."""
        return {
            "operationId": self.operation_id,
            "sourceOrigin": self.source_origin,
            "destinationOrigin": self.destination_origin,
            "targetRevision": self.target_revision,
        }

    @property
    def directory(self) -> Path:
        """Select the single fixed private operation directory."""
        return public.ROOT / "migrations" / self.operation_id


def domain(origin: str) -> str:
    """Accept only canonical HTTPS DNS origins without ports or credentials."""
    public.require(
        len(origin) <= MAX_ORIGIN_BYTES
        and re.fullmatch(r"https://(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,63}", origin),
        "invalid_migration_origin",
    )
    return origin.removeprefix("https://")


def parse_request(value: JsonValue) -> Request:
    """Reject ambiguous or extra transport controls before opening host resources."""
    data = object_value(value)
    public.require(
        set(data) == {"operationId", "sourceOrigin", "destinationOrigin", "targetRevision"},
        "invalid_migration_request",
    )
    operation = string_value(data["operationId"])
    revision = string_value(data["targetRevision"])
    source, destination = (
        string_value(data["sourceOrigin"]),
        string_value(data["destinationOrigin"]),
    )
    public.require(re.fullmatch("[a-f0-9]{32}", operation), "invalid_operation_id")
    public.require(re.fullmatch("[a-f0-9]{40}", revision), "invalid_target_revision")
    public.require(domain(source) != domain(destination), "identical_migration_origins")
    return Request(
        operation_id=operation,
        source_origin=source,
        destination_origin=destination,
        target_revision=revision,
    )


def private(path: Path, *, directory: bool = False, limit: int = MAX_PRIVATE) -> None:
    """Reject links, hardlinks, permissive paths and oversized evidence."""
    public.protected(
        path, directory=directory, modes=(0o700,) if directory else (0o600,), limit=limit
    )
    public.require(path.resolve() == path, "migration_path_symlink")
    if not directory:
        public.require(path.lstat().st_nlink == 1, "migration_file_hardlink")


def read_object(path: Path, *, limit: int = MAX_PRIVATE) -> JsonObject:
    """Read only a bounded protected JSON object."""
    private(path, limit=limit)
    return object_value(decode_json(path.read_bytes()))


def request_file(path: Path) -> Request:
    """Require the bootstrap's fixed request location and private ancestors."""
    request = parse_request(read_object(path, limit=16384))
    public.require(path == request.directory / "request.json", "request_path_mismatch")
    for parent in (public.ROOT, public.CONFIG, public.ROOT / "migrations", request.directory):
        private(parent, directory=True)
    return request


@contextmanager
def workload(request: Request) -> Generator[None]:
    """Share the canonical lock, admitting only this operation's own journal."""
    public.WORK.mkdir(mode=0o700, exist_ok=True)
    private(public.WORK, directory=True)
    descriptor = os.open(
        public.WORK / "workload.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600
    )
    try:
        private(public.WORK / "workload.lock")
        fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        benchmark = public.WORK / "current.json"
        if benchmark.exists() or benchmark.is_symlink():
            public.require(read_object(benchmark).get("finalized") is True, "unfinished_host_work")
        release = public.ROOT / "release-state.json"
        if release.exists() or release.is_symlink():
            value = read_object(release)
            own = (
                value.get("operation") == "server_migration"
                and value.get("request") == request.json()
            )
            public.require(
                value.get("schemaVersion") == 1 and (value.get("finalized") is True or own),
                "unfinished_other_release",
            )
        yield
    finally:
        os.close(descriptor)


def state(request: Request) -> JsonObject | None:
    """Read one journal only when it binds this exact request."""
    path = request.directory / "migration.json"
    if not path.exists() and not path.is_symlink():
        return None
    value = read_object(path)
    public.require(
        value.get("schemaVersion") == 1 and value.get("request") == request.json(),
        "migration_journal_identity",
    )
    return value


def record(
    request: Request, phase: str, details: JsonObject, *, finalized: bool = False
) -> JsonObject:
    """Write durable ownership before changing any runtime lifecycle state."""
    value: JsonObject = {
        **details,
        "schemaVersion": 1,
        "operation": "server_migration",
        "request": request.json(),
        "phase": phase,
        "finalized": finalized,
        "updatedAt": public.timestamp(),
    }
    public.atomic(
        public.ROOT / "release-state.json",
        {
            "schemaVersion": 1,
            "operation": "server_migration",
            "request": request.json(),
            "attempt": str(request.directory),
            "phase": phase,
            "finalized": finalized,
        },
    )
    public.atomic(request.directory / "migration.json", value)
    return value


def configuration() -> tuple[JsonObject, dict[str, str]]:
    """Read identity privately without exposing any runtime secret values."""
    images = read_object(public.CONFIG / "images.json")
    path = public.CONFIG / "app.env"
    private(path)
    environment: dict[str, str] = {}
    for line in path.read_text().splitlines():
        if line and not line.startswith("#"):
            key, separator, value = line.partition("=")
            public.require(
                separator and key not in environment, "ambiguous_application_environment"
            )
            environment[key] = value
    origin = environment.get("ALLOWED_ORIGINS", "")
    _ = domain(origin)
    public.require(
        environment.get("WEBAUTHN_ORIGIN") == origin
        and environment.get("RUN_MIGRATIONS") == "false",
        "invalid_runtime_identity",
    )
    public.require(
        re.fullmatch("[a-f0-9]{40}", string_value(images.get("revision"))), "invalid_image_revision"
    )
    public.require(
        public.ID.fullmatch(string_value(images.get("serverImage"))), "invalid_server_image"
    )
    return images, environment


def configuration_hashes() -> JsonObject:
    """Bind recovery to the mounted configuration, not only Compose labels."""
    result: JsonObject = {}
    for name, mode in {
        "app.env": 0o600,
        "compose.public.yml": 0o600,
        "images.json": 0o600,
        "proxy.env": 0o600,
        "compose.base.yml": 0o644,
        "Caddyfile": 0o644,
        "pg_hba.conf": 0o644,
    }.items():
        path = public.CONFIG / name
        public.protected(path, modes=(mode,), limit=MAX_PRIVATE)
        result[name] = sha256_file(path)
    return result


def owned_container(
    runner: public.RunnerProtocol,
    service: str,
    *,
    optional: bool = False,
    project: str = "simplestchat-public",
) -> JsonObject | None:
    """Inspect only one exact container carrying both expected Compose labels."""
    output = (
        runner.docker(
            "ps",
            "--all",
            "--quiet",
            "--no-trunc",
            "--filter",
            f"label=com.docker.compose.project={project}",
            "--filter",
            f"label=com.docker.compose.service={service}",
            timeout=10,
        )
        .decode()
        .strip()
    )
    if not output and optional:
        return None
    public.require(re.fullmatch("[a-f0-9]{64}", output), "owned_container_count")
    value = object_value(
        decode_json(runner.docker("inspect", "--format", CONTAINER_FORMAT, output, timeout=10))
    )
    public.require(
        value.get("id") == output
        and value.get("project") == project
        and value.get("service") == service
        and value.get("name") == f"/{project}-{service}-1"
        and public.ID.fullmatch(string_value(value.get("image")))
        and value.get("oom") is False,
        "container_ownership_mismatch",
    )
    return value


def sql(
    runner: public.RunnerProtocol, database: str, statement: str, *, readonly: bool = True
) -> bytes:
    """Bound one private socket query; transaction failure never continues."""
    public.require(re.fullmatch("[a-f0-9]{64}", database), "invalid_database_container")
    prefix = "BEGIN READ ONLY;" if readonly else "BEGIN;"
    payload = (
        prefix
        + " SET LOCAL statement_timeout='30s'; SET LOCAL lock_timeout='3s'; "
        + statement
        + "\nCOMMIT;\n"
    ).encode()
    return runner.docker(
        "exec",
        "--interactive",
        "--user",
        "999:999",
        database,
        "timeout",
        "--signal=TERM",
        "--kill-after=2s",
        "40s",
        "psql",
        "--no-psqlrc",
        "--quiet",
        "--tuples-only",
        "--no-align",
        "--set",
        "ON_ERROR_STOP=on",
        "--host",
        migration_snapshot.SOCKET,
        "--username",
        "postgres",
        "--dbname",
        "simplestchat",
        input_data=payload,
        timeout=45,
    )


def database_identity(runner: public.RunnerProtocol, database: str) -> JsonObject:
    """Check logical cluster identity and reject unhandled databases/large objects."""
    value = object_value(decode_json(sql(runner, database, IDENTITY_SQL)))
    public.require(
        re.fullmatch("[0-9]+", string_value(value.get("systemIdentifier"))),
        "database_system_identity",
    )
    public.require(
        value.get("otherDatabases") is False and value.get("largeObjects") == 0,
        "unrelated_database_contents",
    )
    return value


def machine_id() -> str:
    """Identify the actual host independently of its DNS aliases."""
    value = Path("/etc/machine-id").read_text().strip()
    public.require(re.fullmatch("[a-f0-9]{32}", value), "invalid_machine_identity")
    return value


def inspect(runner: public.RunnerProtocol, request: Request, *, source: bool) -> JsonObject:
    """Return only deployment identity and owned lifecycle state."""
    images, environment = configuration()
    expected = request.source_origin if source else request.destination_origin
    public.require(environment["ALLOWED_ORIGINS"] == expected, "host_origin_mismatch")
    if not source:
        public.require(images["revision"] == request.target_revision, "target_revision_mismatch")
    containers: JsonObject = {}
    for service in ("postgres", "simplestchat", "caddy"):
        value = owned_container(runner, service, optional=not source)
        containers[service] = value
    database = containers["postgres"]
    identity: JsonObject = {}
    if isinstance(database, dict) and database.get("running") is True:
        identity = database_identity(runner, string_value(database["id"]))
    result: JsonObject = {
        "machineId": machine_id(),
        "domain": domain(expected),
        "rpId": environment["WEBAUTHN_RP_ID"],
        "revision": images["revision"],
        "databaseSystemIdentifier": identity.get("systemIdentifier"),
        "emptyDatabase": identity.get("emptyDatabase", database is None),
        "postgresVersion": identity.get("serverVersion"),
        "postgresSelector": images.get("postgresImage"),
        "containers": containers,
        "configurationSha256": configuration_hashes(),
    }
    if source:
        public.require(
            all(object_value(value).get("running") is True for value in containers.values()),
            "source_services_not_running",
        )
        public.require(
            object_value(containers["simplestchat"])["image"] == images["serverImage"],
            "source_application_image_mismatch",
        )
    else:
        public.require(result["emptyDatabase"] is True, "target_database_not_empty")
        public.require(
            all(
                value is None or object_value(value).get("running") is False
                for service, value in containers.items()
                if service != "postgres"
            ),
            "target_application_running",
        )
    return result


def units(names: tuple[str, ...]) -> JsonObject:
    """Record only fixed managed unit states, allowing genuinely absent optional units."""
    public.require(set(names).issubset(RETIRE_UNITS), "unowned_managed_unit")
    result: JsonObject = {}
    for name in names:
        status, output, _error = bounded_process.run(
            ["/usr/bin/systemctl", "show", name, "--property=LoadState,ActiveState,UnitFileState"],
            env=public.ENV,
            limits=bounded_process.Limits(timeout=10, stdout=4096, stderr=4096),
        )
        value: dict[str, str] = {}
        for line in output.decode("utf-8", errors="strict").splitlines():
            key, separator, item = line.partition("=")
            public.require(separator and key not in value, "ambiguous_managed_unit")
            value[key] = item
        public.require(
            set(value).issubset({"LoadState", "ActiveState", "UnitFileState"})
            and (
                (
                    value.get("LoadState") == "not-found"
                    and value.get("ActiveState") == "inactive"
                    and not value.get("UnitFileState")
                )
                or (
                    status == 0
                    and value.get("LoadState") == "loaded"
                    and value.get("ActiveState")
                    in ("active", "inactive", "failed", "activating", "deactivating", "reloading")
                    and "UnitFileState" in value
                )
            ),
            "ambiguous_managed_unit",
        )
        result[name] = {
            "loaded": value["LoadState"] == "loaded",
            "active": value.get("ActiveState") in ("active", "activating", "reloading"),
            "enabled": value.get("UnitFileState", ""),
        }
    return result


def unit_names(values: JsonObject, *, active: bool = False) -> list[str]:
    """Return only allowlisted managed units from retained evidence."""
    public.require(set(values).issubset(RETIRE_UNITS), "unowned_unit_in_journal")
    return [
        name
        for name, value in values.items()
        if object_value(value).get("loaded") is True
        and (not active or object_value(value).get("active") is True)
    ]


def stop_units(runner: public.RunnerProtocol, values: JsonObject) -> None:
    """Stop timers and in-flight writers before capturing the database."""
    selected = unit_names(values)
    if selected:
        _ = runner.run(["/usr/bin/systemctl", "stop", *selected], timeout=120)


def stopped(runner: public.RunnerProtocol) -> None:
    """Refuse data operations while either public application endpoint runs."""
    for service in ("simplestchat", "caddy"):
        value = owned_container(runner, service, optional=True)
        public.require(value is None or value.get("running") is False, "public_endpoint_running")


def no_clients(runner: public.RunnerProtocol, database: str) -> None:
    """Require quiescence beyond merely having stopped the main application."""
    raw = sql(
        runner,
        database,
        """SELECT count(*) FROM pg_stat_activity WHERE datname=current_database()
AND pid<>pg_backend_pid() AND backend_type='client backend';""",
    )
    public.require(raw.strip() == b"0", "database_clients_still_present")


def pinned_postgres(runner: public.RunnerProtocol, value: JsonObject, selector: str) -> None:
    """Bind the actual container image to the installed immutable selection."""
    public.require(re.fullmatch(r"[^\s]+@sha256:[a-f0-9]{64}", selector), "postgres_not_pinned")
    identity = (
        runner.docker("image", "inspect", "--format", "{{.Id}}", selector, timeout=10)
        .decode()
        .strip()
    )
    public.require(identity == value.get("image"), "postgres_image_mismatch")


def recover(runner: public.RunnerProtocol, request: Request, prior: JsonObject) -> JsonObject:
    """Restore only recorded source services, and permanently refuse sealed cutovers."""
    public.require(
        prior.get("phase") in ("freezing", "freeze-failed", "frozen", "source-recovered"),
        "source_recovery_forbidden",
    )
    shared = read_object(public.ROOT / "release-state.json")
    public.require(
        shared.get("schemaVersion") == 1
        and shared.get("operation") == "server_migration"
        and shared.get("request") == request.json()
        and shared.get("phase") in ("freezing", "freeze-failed", "frozen", "source-recovered"),
        "source_recovery_forbidden",
    )
    images, environment = configuration()
    before = object_value(prior["before"])
    public.require(
        environment["ALLOWED_ORIGINS"] == request.source_origin
        and images["revision"] == before["revision"],
        "source_recovery_identity",
    )
    public.require(
        configuration_hashes() == before["configurationSha256"], "source_configuration_changed"
    )
    recorded = object_value(before["containers"])
    for service in ("postgres", "simplestchat", "caddy"):
        actual = owned_container(runner, service)
        expected = object_value(recorded[service])
        public.require(
            actual is not None
            and all(actual.get(key) == expected.get(key) for key in ("id", "image", "configHash")),
            "source_container_replaced",
        )
        public.require(
            runner.compose("config", "--hash", service, timeout=15).decode().split()
            == [service, expected["configHash"]],
            "source_configuration_changed",
        )
        if prior.get("phase") == "source-recovered":
            public.require(
                actual is not None and actual.get("running") is True,
                "recovered_source_no_longer_running",
            )
    if prior.get("phase") == "source-recovered":
        public.ready(runner, seconds=30)
        public.ready(runner, origin=request.source_origin, seconds=30)
        return prior
    _ = runner.compose("start", "--wait", "--wait-timeout", "120", "postgres", timeout=130)
    _ = runner.compose("start", "--wait", "--wait-timeout", "120", "simplestchat", timeout=130)
    public.ready(runner, seconds=30)
    _ = runner.compose("start", "caddy", timeout=30)
    public.ready(runner, origin=request.source_origin, seconds=30)
    active = unit_names(object_value(prior["units"]), active=True)
    if active:
        _ = runner.run(["/usr/bin/systemctl", "start", *active], timeout=120)
    return record(request, "source-recovered", prior, finalized=True)


def freeze(runner: public.RunnerProtocol, request: Request) -> JsonObject:
    """Freeze writers, retain a verified archive and restore locally on preparation failure."""
    public.require(state(request) is None, "migration_already_started")
    before = inspect(runner, request, source=True)
    database = object_value(object_value(before["containers"])["postgres"])
    pinned_postgres(runner, database, string_value(before["postgresSelector"]))
    writers = units(DB_UNITS)
    details: JsonObject = {"role": "source", "before": before, "units": writers}
    _ = record(request, "freezing", details)
    try:
        stop_units(runner, writers)
        _ = runner.compose("stop", "--timeout", "30", "simplestchat", timeout=45)
        _ = runner.compose("stop", "--timeout", "3", "caddy", timeout=15)
        stopped(runner)
        database_id = string_value(database["id"])
        no_clients(runner, database_id)
        snapshot = migration_snapshot.collect(database_id)
        public.atomic(request.directory / "database-before.json", snapshot)
        partial = request.directory / "database.dump.partial"
        public.require(
            not partial.exists() and not (request.directory / "database.dump").exists(),
            "migration_archive_exists",
        )
        public.backup_headroom(runner, database_id)
        backup_public.dump_database(runner, database_id, partial)
        public.require(0 < partial.stat().st_size <= MAX_DUMP, "migration_archive_size")
        public.publish_backup(partial, request.directory / "database.dump")
        no_clients(runner, database_id)
        public.require(
            migration_snapshot.collect(database_id) == snapshot, "source_changed_during_dump"
        )
        private(public.CONFIG / "secrets.json")
        archive: JsonObject = {
            "schemaVersion": 1,
            "request": request.json(),
            "source": before,
            "bytes": (request.directory / "database.dump").stat().st_size,
            "sha256": sha256_file(request.directory / "database.dump"),
            "snapshotSha256": sha256_file(request.directory / "database-before.json"),
            "secretsSha256": sha256_file(public.CONFIG / "secrets.json"),
            "postgresImage": database["image"],
            "postgresSelector": before["postgresSelector"],
            "systemIdentifier": snapshot["systemIdentifier"],
            "completedAt": public.timestamp(),
        }
        public.atomic(request.directory / "archive.json", archive)
        return record(request, "frozen", {**details, "archive": archive})
    except Exception:
        failed = record(request, "freeze-failed", details)
        try:
            _ = recover(runner, request, failed)
        except Exception:  # noqa: BLE001 -- Preserve failed recovery durably without changing the original failure.
            _ = record(request, "freeze-failed", details)
        raise


def verified_archive(request: Request) -> tuple[JsonObject, JsonObject]:
    """Verify the exact transferred private bytes before any destination mutation."""
    archive = read_object(request.directory / "archive.json")
    public.require(
        archive.get("schemaVersion") == 1 and archive.get("request") == request.json(),
        "archive_identity",
    )
    dump = request.directory / "database.dump"
    private(dump, limit=MAX_DUMP)
    public.require(
        0 < dump.stat().st_size == integer_value(archive["bytes"])
        and sha256_file(dump) == archive.get("sha256"),
        "archive_digest_mismatch",
    )
    snapshot = read_object(request.directory / "database-before.json")
    public.require(
        sha256_file(request.directory / "database-before.json") == archive.get("snapshotSha256")
        and snapshot.get("passed") is True
        and snapshot.get("systemIdentifier") == archive.get("systemIdentifier"),
        "snapshot_digest_mismatch",
    )
    private(public.CONFIG / "secrets.json")
    public.require(
        sha256_file(public.CONFIG / "secrets.json") == archive.get("secretsSha256"),
        "destination_secrets_differ",
    )
    return archive, snapshot


def owner_sql(source: str, destination: str, system_identifier: str) -> str:
    """Produce one cluster-bound update preserving every non-email owner field."""
    old, new = "owner@" + domain(source), "owner@" + domain(destination)
    public.require(re.fullmatch("[0-9]+", system_identifier), "invalid_target_system_identity")
    # Origin validation permits no apostrophes, backslashes or SQL metacharacters.
    return f"""DO $$ DECLARE original public.users%ROWTYPE; affected bigint; BEGIN
IF current_database()<>'simplestchat' OR current_user<>'postgres'
 OR (pg_control_system()).system_identifier::text<>'{system_identifier}' THEN
 RAISE EXCEPTION 'Destination database identity mismatch'; END IF;
SELECT * INTO STRICT original FROM public.users WHERE email='{old}' FOR UPDATE;
IF EXISTS(SELECT FROM public.users WHERE email='{new}')
 OR NOT EXISTS(SELECT FROM public.rooms WHERE id='lobby' AND owner_id=original.id) THEN
 RAISE EXCEPTION 'Managed owner precondition failed'; END IF;
UPDATE public.users SET email='{new}' WHERE id=original.id AND email='{old}';
GET DIAGNOSTICS affected=ROW_COUNT;
IF affected<>1 OR (SELECT to_jsonb(u)-'email' FROM public.users u WHERE id=original.id)
 IS DISTINCT FROM (to_jsonb(original)-'email') THEN
 RAISE EXCEPTION 'Managed owner preservation failed'; END IF;
END $$;"""  # noqa: S608 -- canonical domains and decimal cluster identifier are validated above.


def retain_destination_owner(request: Request) -> None:
    """Align the private seeder credential with the preserved destination owner."""
    secrets = read_object(public.CONFIG / "secrets.json")
    password = string_value(secrets["owner_password"])
    public.require(re.fullmatch("[a-f0-9]{64}", password), "invalid_owner_secret")
    owner: JsonObject = {
        "email": "owner@" + domain(request.destination_origin),
        "password": password,
    }
    path = public.CONFIG / "owner.json"
    if path.exists() or path.is_symlink():
        current = read_object(path)
        public.require(
            set(current) == {"email", "password"}
            and current.get("email") == owner["email"]
            and hmac.compare_digest(string_value(current.get("password")), password),
            "destination_owner_file_conflict",
        )
    else:
        public.atomic(path, owner)


def expected_owner_snapshot(database: str, request: Request, restored: JsonObject) -> JsonObject:
    """Bind the sole permitted database difference to an independently streamed digest."""
    result = deepcopy(restored)
    matches = 0
    for value in array_value(result["tables"]):
        table = object_value(value)
        if table.get("schema") == "public" and table.get("name") == "users":
            matches += 1
            table.update(
                migration_snapshot.renamed_users(
                    database,
                    "owner@" + domain(request.source_origin),
                    "owner@" + domain(request.destination_origin),
                )
            )
    public.require(matches == 1, "users_snapshot_missing")
    return result


def restore(runner: public.RunnerProtocol, request: Request) -> JsonObject:
    """Restore into an empty owned database, prove equivalence, then rename only its owner."""
    public.require(state(request) is None, "migration_already_started")
    archive, snapshot = verified_archive(request)
    before = inspect(runner, request, source=False)
    public.require(
        before["machineId"] != object_value(archive["source"])["machineId"],
        "identical_migration_hosts",
    )
    public.require(
        before["rpId"] == object_value(archive["source"])["rpId"], "passkey_rp_id_changed"
    )
    details: JsonObject = {"role": "destination", "before": before, "archive": archive}
    _ = record(request, "restoring", details)
    _ = runner.compose(
        "up",
        "--detach",
        "--no-build",
        "--pull",
        "never",
        "--wait",
        "--wait-timeout",
        "120",
        "postgres",
        timeout=135,
    )
    database = owned_container(runner, "postgres")
    public.require(database is not None, "destination_database_missing")
    if database is None:
        message = "destination_database_missing"
        raise public.ReleaseError(message)
    identity = database_identity(runner, string_value(database["id"]))
    public.require(identity["emptyDatabase"] is True, "target_database_not_empty")
    public.require(
        identity["systemIdentifier"] != archive["systemIdentifier"], "same_database_cluster"
    )
    public.require(
        identity["serverVersion"] == snapshot["postgresVersion"], "postgres_version_mismatch"
    )
    public.require(
        before["postgresSelector"] == archive["postgresSelector"]
        and database["image"] == archive["postgresImage"],
        "postgres_image_mismatch",
    )
    pinned_postgres(runner, database, string_value(archive["postgresSelector"]))
    stopped(runner)
    database_id = string_value(database["id"])
    no_clients(runner, database_id)
    _ = runner.docker(
        "exec",
        "--interactive",
        "--user",
        "999:999",
        database_id,
        "timeout",
        "--signal=TERM",
        "--kill-after=2s",
        "120s",
        "pg_restore",
        "--exit-on-error",
        "--single-transaction",
        "--host",
        migration_snapshot.SOCKET,
        "--username",
        "postgres",
        "--dbname",
        "simplestchat",
        input_path=request.directory / "database.dump",
        timeout=130,
    )
    restored = migration_snapshot.collect(database_id)
    public.atomic(request.directory / "database-restored.json", restored)
    migration_snapshot.equivalent(snapshot, restored)
    expected = expected_owner_snapshot(database_id, request, restored)
    _ = sql(
        runner,
        database_id,
        owner_sql(
            request.source_origin,
            request.destination_origin,
            string_value(identity["systemIdentifier"]),
        ),
        readonly=False,
    )
    retain_destination_owner(request)
    stopped(runner)
    renamed = migration_snapshot.collect(database_id)
    public.require(renamed == expected, "unexpected_owner_rename_changes")
    public.atomic(request.directory / "database-renamed.json", renamed)
    return record(
        request,
        "restored",
        {
            **details,
            "databaseSystemIdentifier": identity["systemIdentifier"],
            "databaseEquivalentBeforeOwnerRename": True,
            "ownerFieldsPreserved": True,
            "renamedSnapshotSha256": sha256_file(request.directory / "database-renamed.json"),
        },
        finalized=True,
    )


def verify_target(runner: public.RunnerProtocol, request: Request, prior: JsonObject) -> JsonObject:
    """Refuse cutover until the exact restored destination remains unchanged and stopped."""
    public.require(
        prior.get("role") == "destination" and prior.get("phase") == "restored",
        "destination_not_restored",
    )
    archive, _snapshot = verified_archive(request)
    images, environment = configuration()
    public.require(
        images["revision"] == request.target_revision
        and environment["ALLOWED_ORIGINS"] == request.destination_origin
        and environment["WEBAUTHN_RP_ID"] == object_value(archive["source"])["rpId"],
        "destination_identity_changed",
    )
    public.require(
        public.image_identity(runner, string_value(images["serverImage"]), request.target_revision)
        == images["serverImage"],
        "destination_image_revision_mismatch",
    )
    stopped(runner)
    database = owned_container(runner, "postgres")
    if database is None:
        message = "destination_database_missing"
        raise public.ReleaseError(message)
    public.require(database["image"] == archive["postgresImage"], "postgres_image_mismatch")
    expected = read_object(request.directory / "database-renamed.json")
    public.require(
        sha256_file(request.directory / "database-renamed.json")
        == prior.get("renamedSnapshotSha256"),
        "renamed_snapshot_changed",
    )
    no_clients(runner, string_value(database["id"]))
    public.require(
        migration_snapshot.collect(string_value(database["id"])) == expected,
        "restored_database_changed",
    )
    return {"phase": "target-verified", "databaseSystemIdentifier": expected["systemIdentifier"]}


def seal(runner: public.RunnerProtocol, request: Request, prior: JsonObject) -> JsonObject:
    """Irreversibly remove source recovery before destination startup is attempted."""
    public.require(prior.get("phase") == "frozen", "source_not_frozen")
    stopped(runner)
    _archive, snapshot = verified_archive(request)
    database = owned_container(runner, "postgres")
    if database is None:
        message = "source_database_missing"
        raise public.ReleaseError(message)
    no_clients(runner, string_value(database["id"]))
    public.require(
        migration_snapshot.collect(string_value(database["id"])) == snapshot,
        "frozen_source_changed",
    )
    return record(request, "cutover-sealed", prior)


def retire(runner: public.RunnerProtocol, request: Request, prior: JsonObject) -> JsonObject:
    """Stop only old deployment resources, disable its timers, and retain every volume."""
    public.require(
        prior.get("phase") in ("cutover-sealed", "retiring", "retired"), "source_not_sealed"
    )
    _ = record(request, "retiring", prior)
    selected = units(RETIRE_UNITS)
    stop_units(runner, selected)
    timers = [name for name in unit_names(selected) if name.endswith(".timer")]
    if timers:
        _ = runner.run(["/usr/bin/systemctl", "disable", *timers], timeout=30)
    for service in ("simplestchat", "caddy", "postgres"):
        value = owned_container(runner, service)
        public.require(value is not None, "retired_container_missing")
        if value is not None:
            _ = runner.docker(
                "stop",
                "--time",
                "30" if service != "caddy" else "3",
                string_value(value["id"]),
                timeout=45,
            )
            _ = runner.docker("update", "--restart=no", string_value(value["id"]), timeout=15)
    for project, service in (
        ("simplestchat-turn", "turn"),
        ("simplestchat-monitoring", "prometheus"),
        ("simplestchat-monitoring", "node-exporter"),
    ):
        auxiliary = owned_container(runner, service, optional=True, project=project)
        if auxiliary is not None:
            _ = runner.docker("stop", "--time", "10", string_value(auxiliary["id"]), timeout=20)
            _ = runner.docker("update", "--restart=no", string_value(auxiliary["id"]), timeout=15)
    return record(request, "retired", prior)


def retain_failed_database(request: Request) -> None:
    """Move a stopped failed copy into its private attempt without deleting any data."""
    database = public.ROOT / "postgres"
    retained = request.directory / "retained-postgres"
    for path in (database, retained):
        if path.exists() or path.is_symlink():
            metadata = path.lstat()
            public.require(
                stat.S_ISDIR(metadata.st_mode)
                and metadata.st_uid == POSTGRES_UID
                and stat.S_IMODE(metadata.st_mode) == POSTGRES_DIRECTORY_MODE
                and path.resolve() == path,
                "unsafe_failed_database_directory",
            )
    if retained.exists():
        public.require(
            not database.exists() or not any(database.iterdir()), "replacement_database_not_empty"
        )
    else:
        public.require(database.is_dir(), "failed_database_missing")
        _ = database.rename(retained)
    if not database.exists():
        database.mkdir(mode=0o700)
        os.chown(database, POSTGRES_UID, POSTGRES_UID)


def abort_target(runner: public.RunnerProtocol, request: Request, prior: JsonObject) -> JsonObject:
    """Retain an unsuccessful restore only when no destination app has ever been created."""
    public.require(
        prior.get("role") == "destination" and prior.get("phase") in ("restoring", "aborting"),
        "destination_abort_phase",
    )
    shared = read_object(public.ROOT / "release-state.json")
    public.require(
        shared.get("operation") == "server_migration"
        and shared.get("request") == request.json()
        and shared.get("phase") == prior["phase"]
        and shared.get("finalized") is False,
        "destination_abort_journal",
    )
    public.require(
        configuration_hashes() == object_value(prior["before"])["configurationSha256"],
        "destination_abort_configuration_changed",
    )
    for service in ("simplestchat", "caddy"):
        public.require(
            owned_container(runner, service, optional=True) is None,
            "destination_application_was_created",
        )
    database = owned_container(runner, "postgres", optional=True)
    if prior["phase"] == "restoring":
        public.require(database is not None, "failed_database_container_missing")
    details: JsonObject = {**prior}
    if database is not None:
        container = string_value(database["id"])
        public.require(
            prior["phase"] == "restoring" or prior.get("abortedContainer") == container,
            "destination_abort_container_changed",
        )
        mounts = array_value(
            decode_json(
                runner.docker("inspect", "--format", "{{json .Mounts}}", container, timeout=10)
            )
        )
        public.require(
            any(
                object_value(mount).get("Type") == "bind"
                and object_value(mount).get("Source") == str(public.ROOT / "postgres")
                and object_value(mount).get("Destination") == "/var/lib/postgresql"
                for mount in mounts
            ),
            "destination_abort_database_mount",
        )
        details["abortedContainer"] = container
    _ = record(request, "aborting", details)
    if database is not None:
        container = string_value(database["id"])
        _ = runner.docker("stop", "--time", "30", container, timeout=45)
        _ = runner.docker("update", "--restart=no", container, timeout=15)
        _ = runner.docker("rm", container, timeout=15)
    retain_failed_database(request)
    return record(request, "aborted", details, finalized=True)


def execute(action: str, request: Request, runner: public.RunnerProtocol) -> JsonObject:
    """Dispatch fixed actions; no host-side action starts the destination application."""
    if action in ("inspect-source", "inspect-target"):
        return inspect(runner, request, source=action == "inspect-source")
    if action == "freeze-source":
        return freeze(runner, request)
    if action == "restore-target":
        return restore(runner, request)
    prior = state(request)
    public.require(prior is not None, "migration_journal_missing")
    if prior is None:
        message = "migration_journal_missing"
        raise public.ReleaseError(message)
    handlers: dict[str, Callable[[public.RunnerProtocol, Request, JsonObject], JsonObject]] = {
        "verify-target": verify_target,
        "recover-source": recover,
        "seal-source": seal,
        "finalize-source": retire,
        "abort-target": abort_target,
    }
    handler = handlers.get(action)
    if handler is None:
        message = "unsupported_migration_action"
        raise public.ReleaseError(message)
    if action not in ("verify-target", "abort-target"):
        public.require(prior.get("role") == "source", "source_action_on_destination")
    return handler(runner, request, prior)


class Arguments(argparse.Namespace):
    """Expose only the fixed action and canonical request path."""

    action: str = ""
    request: str = ""


def main() -> int:
    """Run one bounded private action and report no secret-bearing exception text."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("action", choices=ACTIONS)
    _ = parser.add_argument("--request", required=True)
    args = parser.parse_args(namespace=Arguments())

    def interrupted(_signum: int, _frame: FrameType | None) -> NoReturn:
        message = "migration_interrupted"
        raise public.ReleaseError(message)

    try:
        public.require(os.geteuid() == 0, "root_required")
        _ = os.umask(0o077)
        for signum in (signal.SIGTERM, signal.SIGINT):
            _ = signal.signal(signum, interrupted)
        request = request_file(Path(args.request))
        with workload(request):
            logs = Path(tempfile.mkdtemp(prefix=args.action + ".", dir=request.directory))
            result = execute(args.action, request, public.Runner(logs))
            report: JsonObject = {
                "passed": True,
                "action": args.action,
                "operationId": request.operation_id,
                "phase": result.get("phase"),
                "result": result,
            }
            _ = sys.stdout.write(json.dumps(report) + "\n")
    except Exception:  # noqa: BLE001 -- Fixed CLI boundary must never expose secret-bearing unexpected exceptions.
        _ = sys.stderr.write('{"passed":false,"error":"migration_action_failed"}\n')
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
