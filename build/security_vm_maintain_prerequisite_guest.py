"""Own one fixed unchanged900 launcher prerequisite trial in its disposable guest.

This keeper is independent of the product maintain implementation. Native run/init
owns PID namespaces and reaping; this module owns real fixture inputs and daemon
observations under its original outer clock. Runtime remains unproved until run.
"""

from __future__ import annotations

import fcntl
import hashlib
import json
import math
import os
import re
import secrets
import select
import signal
import ssl
import stat
import subprocess
import sys
import time
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import TYPE_CHECKING, cast, final

ROOT = Path("/root/simplestchat-vm-fixture")
PRIVATE = ROOT / "maintain-prerequisite"
LIBRARY = Path("/usr/local/libexec/simplestchat-public")
BENCH_LIBRARY = Path("/usr/local/libexec/simplestchat-bench")
PUBLIC = Path("/srv/simplestchat-public")
CONFIG = Path("/etc/simplestchat-public")
WORK = Path("/run/simplestchat-bench")
INSTALLED = LIBRARY / "simplestchat-ops"
SOURCE_ROOT = Path("/srv/simplestchat-bench/sources")
ENV = {"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LC_ALL": "C", "HOME": str(ROOT)}
NAME = "maintain.localhost"
SECONDS = 1800
FINALIZATION = 120
SETUP = 510
NATIVE = 1050
NATIVE_FINALIZATION = 60
OBSERVATION = 120
OBSERVATION_CAPS = {
    "daemon": 15,
    "database": 15,
    "tls": 10,
    "application": 5,
    "source": 10,
    "collector": 15,
    "actors": 15,
    "network": 15,
    "guard": 10,
}
LOCAL_OBSERVATION_MARGIN = 10
DEBT = NATIVE + OBSERVATION
DEADLINE_STATUS = 124
MAX_RECORD = 131072
MAX_STREAM = 2 * 1024**2
MAX_BUNDLE = 320 * 1024**2
PRIVATE_MODE = 0o600
EXECUTABLE_MODE = 0o755
PIPE_COUNT = 2
MAX_EVENTS = 2048
CADDY_PIDS = 128
CADDY_NANO_CPUS = 500000000
POSITIVE_APP_ACTORS = 2
DIRECTORY_MODE = 0o700
CASES = frozenset({"positive", "dirty-source", "cancel-owned-launcher", "timeout-stopped-launcher"})
TRIALS = {
    "positive": "normal",
    "dirty-source": "normal",
    "cancel-owned-launcher": "cancel-owned-launcher",
    "timeout-stopped-launcher": "timeout-stopped-launcher",
}
EXPECTED_STATUS = {
    "positive": 0,
    "dirty-source": 1,
    "cancel-owned-launcher": 143,
    "timeout-stopped-launcher": 124,
}

# Isolated execution imports only fixed root-staged originals.
if __name__ == "__main__":
    sys.path[:0] = [str(ROOT), str(LIBRARY)]
import bounded_process  # noqa: E402 -- Fixed staged helper after isolated path selection.
import caddy_materials  # noqa: E402 -- Fixed staged helper after isolated path selection.
import caddy_probe_guest as caddy  # noqa: E402 -- Fixed staged helper.
import release_artifact as artifact  # noqa: E402 -- Fixed staged helper.
import release_assets  # noqa: E402 -- Fixed staged helper.
import security_vm_ops_guest as ops  # noqa: E402 -- Fixed staged owners.
import security_vm_stage_guest as stage  # noqa: E402 -- Fixed staged owners.
import yaml  # noqa: E402 -- Guest dependency admitted before helper start.
from jinja2 import Environment, StrictUndefined  # noqa: E402 -- Fixed template adapter.
from release_json import JsonValue, decode_json, json_value, object_value  # noqa: E402

if TYPE_CHECKING:
    from collections.abc import Callable, Generator, Mapping, Sequence
    from typing import BinaryIO

require = ops.require


def required[T](value: T | None) -> T:
    """Refuse absent private phase ownership before borrowing it."""
    if value is None:
        require(None)
        message = "maintain_prerequisite_missing_owner"
        raise ValueError(message)
    return value


def dictionary(value: object) -> dict[str, object]:
    """Validate object/key types before inspecting bounded private engine fields."""
    require(isinstance(value, dict))
    result = cast("dict[object, object]", value)
    require(all(isinstance(name, str) for name in result))
    return cast("dict[str, object]", result)


def exact(value: object, fields: set[str]) -> dict[str, object]:
    """Admit only a closed object before using its fields or projecting it."""
    return stage.exact(value, fields)


def digest(value: object, count: int = 64) -> str:
    """Validate an exact lowercase private identity without changing its bytes."""
    return stage.digest(value, count)


def integer(value: object, low: int, high: int) -> int:
    """Retain bool-versus-int and range distinctions."""
    return stage.integer(value, low, high)


def encoded(value: object) -> bytes:
    """Bound one closed fixed protocol frame."""
    data = json.dumps(value, separators=(",", ":"), allow_nan=False).encode() + b"\n"
    require(len(data) <= MAX_RECORD)
    return data


class ControlRefusalError(ValueError):
    """Retain the closed status observed by the original control check, before later cleanup."""


@final
class Control:
    """Keep entry-time setup/body/end and first signal without a refreshed clock."""

    def __init__(self, started: float) -> None:
        """Permit only bounded initial input before decoding remaining durations."""
        self.started = started
        self.setup_end = started + 30
        self.body = started + 1680
        self.end = self.body + 30
        self.setup = True
        self.settling = False
        self.recovering = False
        self.phase_end: float | None = None
        self.status = 0
        self.checkpoint = "input"

    @property
    def body_end(self) -> float:
        """Keep seed work inside setup until the final live GO admission."""
        return min(self.setup_end, self.body) if self.setup else self.body

    def signal(self, number: int, _frame: object) -> None:
        """Latch cancellation until all known owners settle."""
        self.status = self.status or (130 if number == signal.SIGINT else 143)

    def check(self) -> None:
        """Fence original time and cancellation without pretending an I/O hard wall."""
        now = time.monotonic()
        require(math.isfinite(now) and self.started <= now)
        ceiling = self.end if self.settling else self.body if self.recovering else self.body_end
        if not self.settling and self.phase_end is not None:
            ceiling = min(ceiling, self.phase_end)
        if now >= ceiling:
            self.status = self.status or 124
            raise ControlRefusalError(self.status)
        if self.status and not self.settling and not self.recovering:
            raise ControlRefusalError(self.status)

    def remaining(self, maximum: float) -> float:
        """Borrow the same live original phase for mature fixed command adapters."""
        self.check()
        ceiling = self.end if self.settling else self.body if self.recovering else self.body_end
        if not self.settling and self.phase_end is not None:
            ceiling = min(ceiling, self.phase_end)
        return min(maximum, ceiling - time.monotonic())

    @contextmanager
    def observation(self, maximum: float) -> Generator[None]:
        """Narrow one fixed observation inside the same live owner; never reset its origin."""
        self.check()
        previous = self.phase_end
        self.phase_end = min(time.monotonic() + maximum, self.body, previous or self.body)
        try:
            yield
            self.check()
        finally:
            self.phase_end = previous

    def configure(self, value: Mapping[str, object]) -> None:
        """Narrow from controller remaining durations while keeping the same entry."""
        self.check()
        self.setup_end = self.started + integer(value["setupSeconds"], 1, SETUP)
        self.body = self.started + integer(value["bodySeconds"], DEBT, 1680)
        self.end = self.started + integer(value["endSeconds"], DEBT, SECONDS)
        require(0 < self.end - self.body <= FINALIZATION)
        self.check()

    def advance(self) -> None:
        """Check setup and full remaining debt BEFORE widening this phase."""
        self.check()
        require(self.setup and self.body - time.monotonic() >= DEBT)
        self.check()
        self.setup = False


@final
class Wire:
    """Own one duplicated stdin while restoring its original shared-OFD flags."""

    def __init__(self, control: Control) -> None:
        """Register the duplicate before any fallible flag mutation."""
        self.control, self.buffer, self.fd = control, bytearray(), -1
        self.flags: int | None = None
        control.check()
        self.fd = os.dup(sys.stdin.fileno())
        try:
            self.flags = fcntl.fcntl(self.fd, fcntl.F_GETFL)
            _ = fcntl.fcntl(self.fd, fcntl.F_SETFL, self.flags | os.O_NONBLOCK)
            control.check()
        except BaseException as error:
            first = ops.settle((self.close,), error)
            if first is not None:
                raise first from None
            raise

    def line(self) -> bytes:
        """Read one physical bounded LF frame with original post-read fences."""
        while True:
            self.control.check()
            if b"\n" in self.buffer:
                line, _, rest = self.buffer.partition(b"\n")
                self.buffer = bytearray(rest)
                return bytes(line)
            require(len(self.buffer) < MAX_RECORD)
            try:
                chunk = os.read(self.fd, MAX_RECORD + 1 - len(self.buffer))
            except (BlockingIOError, InterruptedError):
                _ = select.select([self.fd], [], [], 0.05)
                continue
            self.control.check()
            require(chunk)
            self.buffer.extend(chunk)
            require(len(self.buffer) <= MAX_RECORD)

    def no_extra(self) -> None:
        """Reject already delivered extra commands; future data supplies no authority."""
        self.control.check()
        require(not self.buffer)
        try:
            _ = os.read(self.fd, 1)
        except (BlockingIOError, InterruptedError):
            self.control.check()
            return
        self.control.check()
        # Either unexpected bytes or peer EOF invalidates a positive admission.
        require(None)

    def emit(self, value: object) -> None:
        """Keep both actual writes and flush inside the original phase."""
        self.control.check()
        data = encoded(value)
        require(sys.stdout.buffer.write(data) == len(data))
        _ = sys.stdout.buffer.flush()
        self.control.check()

    def close(self) -> None:
        """Attempt original flag restoration and duplicate close independently."""
        if self.fd < 0:
            return
        descriptor, self.fd = self.fd, -1
        actions: list[Callable[[], None]] = []
        flags = self.flags
        if flags is not None:

            def restore() -> None:
                _ = fcntl.fcntl(descriptor, fcntl.F_SETFL, flags)

            actions.append(restore)
        actions.append(lambda: os.close(descriptor))
        first = ops.settle(actions)
        if first is not None:
            raise first


def absent(path: Path, control: Control) -> bool:
    """Accept only an actual no-follow missing entry, never a dangling symlink."""
    control.check()
    try:
        _ = path.lstat()
    except FileNotFoundError:
        control.check()
        return True
    control.check()
    return False


def classification(error: BaseException, _status: int) -> str:
    """Classify the retained first object; later exit status cannot replace that cause."""
    if type(error) is NativeDeadlineError:
        return "deadline"
    if type(error) is NativeStatusError:
        return "native-status"
    if type(error) is ControlRefusalError:
        arguments = cast("tuple[object, ...]", error.args)
        if len(arguments) == 1 and type(arguments[0]) is int:
            if arguments[0] == DEADLINE_STATUS:
                return "deadline"
            if arguments[0] in (130, 143):
                return "cancelled"
        return "refused"
    return "io" if isinstance(error, OSError) else "refused"


def write_private(path: Path, data: bytes, control: Control) -> None:
    """Create0600 and fence each bounded physical write/flush/fsync before checked close."""
    control.check()
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    stream = stage.adopt(descriptor, "wb")
    first: BaseException | None = None
    try:
        control.check()
        for offset in range(0, len(data), 65536):
            control.check()
            chunk = data[offset : offset + 65536]
            require(stream.write(chunk) == len(chunk))
            control.check()
        control.check()
        stream.flush()
        control.check()
        os.fsync(stream.fileno())
        control.check()
    except BaseException as error:  # noqa: BLE001 -- Settle all admitted peers.
        first = error
    finally:
        first = ops.settle((stream.close,), first)
    if first is not None:
        raise first


def json_record(path: Path, control: Control, originals: list[stage.Original]) -> dict[str, object]:
    """Read one retained private original rather than reopening its JSON path."""
    original = stage.Original(
        path, MAX_RECORD, 0o600, cast("stage.Control", cast("object", control))
    )
    originals.append(original)
    return ops.object_json(original.data())


LABEL = "simplestchat.maintain-prerequisite"
PROJECT = "simplestchat-public"
BUNDLE = ROOT / "maintain-source.bundle"
PAYLOAD_ROOT = PRIVATE / "server-input"
TEMPLATE_NAMES = (
    ("app.env", "app.env"),
    ("migration.env", "migration.env"),
    ("proxy.env", "proxy.env"),
    ("compose.public.yml", "compose.yml"),
    ("pg_hba.conf", "pg_hba.conf"),
    ("init-database.sql", "init-database.sql"),
    ("runtime-grants.sql", "runtime-grants.sql"),
    ("postgres-admin-password", "postgres-admin-password"),
)
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
SOURCE_ROLES = {
    "/usr/local/bin/simplestchat-public-deploy": ("ops/ansible/files/deploy-public.sh", 0o755),
    "/usr/local/bin/simplestchat-public": ("ops/ansible/files/public-compose.sh", 0o755),
    str(BENCH_LIBRARY / "seed-public-room.py"): ("ops/ansible/files/seed-public-room.py", 0o755),
    str(BENCH_LIBRARY / "seed_public_room.py"): ("ops/ansible/files/seed_public_room.py", 0o644),
    str(BENCH_LIBRARY / "release_json.py"): ("ops/ansible/files/release_json.py", 0o644),
    str(LIBRARY / "runtime_profile.py"): ("ops/ansible/files/runtime_profile.py", 0o644),
    str(LIBRARY / "release_json.py"): ("ops/ansible/files/release_json.py", 0o644),
}
MIGRATIONS = (
    "migrations/001_create_users.sql",
    "migrations/002_create_sessions.sql",
    "migrations/003_create_rooms.sql",
    "migrations/004_create_room_roles.sql",
    "migrations/005_create_room_states.sql",
    "migrations/006_add_session_token_index.sql",
    "migrations/007_add_webauthn_credential_id.sql",
    "migrations/008_canonicalize_user_emails.sql",
    "migrations/009_add_room_query_indexes.sql",
    "migrations/010_add_refresh_token_families.sql",
    "migrations/011_add_room_search_trigram_indexes.sql",
    "migrations/012_canonicalize_guest_ipv6_moderation_ips.sql",
    "migrations/013_community_accounts.sql",
    "migrations/014_create_room_reports.sql",
    "migrations/015_add_user_chat_style.sql",
    "migrations/016_constraints_and_indexes.sql",
    "migrations/017_moderation_history.sql",
    "migrations/018_user_chat_preferences.sql",
    "migrations/019_invites.sql",
    "migrations/020_invite_receipts_and_retention.sql",
    "migrations/021_invitation_secrets.sql",
    "migrations/022_require_current_sessions.sql",
    "migrations/023_independent_appearance.sql",
    "migrations/024_validate_independent_appearance.sql",
    "migrations/025_chat_history.sql",
    "migrations/026_validate_chat_moderation.sql",
    "migrations/027_chat_updates.sql",
    "migrations/028_chat_lookup_indexes.sql",
    "migrations/029_chat_reply_index.sql",
    "migrations/030_push_subscriptions.sql",
    "migrations/031_discovery.sql",
    "migrations/032_notification_preferences.sql",
    "migrations/033_attachments.sql",
    "migrations/034_room_image_version.sql",
    "migrations/035_role_numbering.sql",
    "migrations/036_validate_role_numbering.sql",
)
STAGED_SOURCES = frozenset(
    {
        "build/security_vm_maintain_prerequisite_guest.py",
        "build/security_vm_stage_guest.py",
        "build/caddy_probe_guest.py",
        "build/caddy_materials.py",
    }
)
PROJECT_SOURCES = frozenset(
    {
        "Caddyfile",
        "docker-compose.yml",
        *[value[0] for value in SOURCE_ROLES.values()],
        *["ops/ansible/templates/public-" + name + ".j2" for _, name in TEMPLATE_NAMES],
        *MIGRATIONS,
    }
)
GUEST_SOURCES = STAGED_SOURCES | PROJECT_SOURCES

BOOLEAN_FACTS = frozenset(
    {
        "namespaceMapped",
        "pidfdMapped",
        "credentialsMatched",
        "privateGo",
        "deadlinesInherited",
        "launcherStarted",
        "nestedClientObserved",
        "launcherStopped",
        "termSeen",
        "workCapExpired",
        "leaderReaped",
        "echild",
        "stdoutEof",
        "stderrEof",
        "stdoutClosed",
        "stderrClosed",
        "capabilitiesClosed",
        "toolsUnchanged",
        "wrapperReaped",
        "wrapperNaturalExit",
        "initControlFlagsRestored",
        "runControlFlagsRestored",
        "chldRegistrationClosed",
        "wrapperStdoutEof",
        "wrapperStderrEof",
        "wrapperStdoutClosed",
        "wrapperStderrClosed",
    }
)
HASH_FACTS = frozenset(
    {
        "stdoutSha256",
        "stderrSha256",
        "opsSha256",
        "unshareSha256",
        "launcherSha256",
        "timeoutSha256",
    }
)
NATIVE_PHASES = frozenset(
    {
        "admission",
        "sources",
        "ready",
        "go",
        "launcher",
        "client",
        "stopped",
        "settle",
        "result",
        "complete",
    }
)
NATIVE_FAILURES = frozenset(
    {
        "input",
        "policy",
        "changed",
        "io",
        "close",
        "deadline",
        "interrupt",
        "terminate",
        "bounds",
        "command",
    }
)
NATIVE_FACTS = (
    BOOLEAN_FACTS
    | HASH_FACTS
    | {
        "adoptedReaped",
        "adoptedRawStatuses",
        "wrapperRawStatus",
        "launcherRawStatus",
        "stdoutBytes",
        "stderrBytes",
        "launcherWorkElapsedNs",
    }
)


def record(data: bytes) -> dict[str, object]:
    """Decode strict bounded byte JSON without accepting duplicate or nonfinite fields."""
    require(0 < len(data) <= MAX_RECORD)
    return dictionary(decode_json(data))


def request(data: bytes) -> dict[str, object]:
    """Admit only exact current signed payloads and narrowing remaining fixture durations."""
    value = exact(
        record(data),
        {
            "schemaVersion",
            "scope",
            "runId",
            "revision",
            "scenario",
            "setupSeconds",
            "bodySeconds",
            "endSeconds",
            "binary",
            "manifest",
            "archive",
            "sourceBundle",
            "sourceTree",
            "sourcePins",
            "helperPins",
            "postgresImage",
            "caddyImage",
            "engineVersion",
        },
    )
    require(type(value["schemaVersion"]) is int and value["schemaVersion"] == 1)
    require(value["scope"] == "MaintainLauncherPrerequisiteRequest" and value["scenario"] in CASES)
    _ = digest(value["runId"], 32)
    _ = digest(value["revision"], 40)
    _ = digest(value["sourceTree"], 40)
    _ = integer(value["setupSeconds"], 1, SETUP)
    _ = integer(value["bodySeconds"], DEBT, SECONDS - FINALIZATION)
    _ = integer(value["endSeconds"], DEBT, SECONDS)
    for key, maximum in (
        ("binary", ops.MAX_BINARY),
        ("manifest", MAX_RECORD),
        ("archive", 2 * 1024**3),
        ("sourceBundle", MAX_BUNDLE),
    ):
        pin = exact(value[key], {"sha256", "bytes"})
        _ = digest(pin["sha256"])
        _ = integer(pin["bytes"], 1, maximum)
    pins = exact(value["sourcePins"], set(GUEST_SOURCES))
    for checksum in pins.values():
        _ = digest(checksum)
    helper_pins = exact(
        value["helperPins"], set(ops.BASE | ops.FETCH | ops.FORCE) | {"security_vm_ops_guest.py"}
    )
    for checksum in helper_pins.values():
        _ = digest(checksum)
    for name in ("postgresImage", "caddyImage"):
        item = value[name]
        require(type(item) is str and re.fullmatch(r"[a-z0-9./:-]+@sha256:[a-f0-9]{64}", item))
    require(value["caddyImage"] == caddy.IMAGE)
    require(
        type(value["engineVersion"]) is str
        and re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", value["engineVersion"])
    )
    return value


def native_frame(data: bytes) -> dict[str, object]:
    """Keep truthful closed refusal facts, including null incomplete hashes and large counters."""
    value = exact(
        record(data),
        {"schemaVersion", "experiment", "trial", "passed", "phase", "failure", "facts"},
    )
    require(type(value["schemaVersion"]) is int and value["schemaVersion"] == 1)
    require(value["experiment"] == "maintain-launcher-prerequisite")
    require(type(value["trial"]) is str and value["trial"] in set(TRIALS.values()))
    require(type(value["passed"]) is bool)
    require(type(value["phase"]) is str and value["phase"] in NATIVE_PHASES)
    failure = value["failure"]
    require(failure is None or (type(failure) is str and failure in NATIVE_FAILURES))
    require(value["passed"] is (failure is None))
    facts = exact(value["facts"], set(NATIVE_FACTS))
    require(all(type(facts[name]) is bool for name in BOOLEAN_FACTS))
    for name in HASH_FACTS:
        if facts[name] is not None:
            _ = digest(facts[name])
    for name in ("stdoutBytes", "stderrBytes"):
        _ = integer(facts[name], 0, (1 << 64) - 1)
    adopted = integer(facts["adoptedReaped"], 0, 2**32 - 1)
    statuses = facts["adoptedRawStatuses"]
    require(type(statuses) is list and len(cast("list[object]", statuses)) == min(adopted, 256))
    for raw in cast("list[object]", statuses):
        _ = integer(raw, -127, 255)
    for name in ("wrapperRawStatus", "launcherRawStatus"):
        if facts[name] is not None:
            _ = integer(facts[name], -127, 255)
    if facts["launcherWorkElapsedNs"] is not None:
        _ = integer(facts["launcherWorkElapsedNs"], 0, NATIVE * 10**9)
    return value


def require_native_case(value: Mapping[str, object], status: int, scenario: str) -> None:
    """Join expected trial semantics without inventing external tool or execution authority."""
    require(scenario in CASES and status == EXPECTED_STATUS[scenario])
    require(value["trial"] == TRIALS[scenario] and value["passed"] is (status == 0))
    expected_failure = {
        "positive": None,
        "dirty-source": "command",
        "cancel-owned-launcher": "terminate",
        "timeout-stopped-launcher": "deadline",
    }[scenario]
    require(value["failure"] == expected_failure)
    require(value["phase"] == ("complete" if scenario == "positive" else "result"))
    facts = dictionary(value["facts"])
    for name in HASH_FACTS:
        _ = digest(facts[name])
    for name in ("stdoutBytes", "stderrBytes"):
        _ = integer(facts[name], 0, MAX_STREAM)
    _ = integer(facts["adoptedReaped"], 0, 256)
    require(integer(facts["wrapperRawStatus"], -127, 255) == EXPECTED_STATUS[scenario])
    launcher = integer(facts["launcherRawStatus"], -127, 255)
    elapsed = integer(facts["launcherWorkElapsedNs"], 0, NATIVE * 10**9)
    required_facts = BOOLEAN_FACTS - {
        "nestedClientObserved",
        "launcherStopped",
        "termSeen",
        "workCapExpired",
    }
    require(all(facts[name] is True for name in required_facts))
    require(facts["termSeen"] is (scenario == "cancel-owned-launcher"))
    require(facts["workCapExpired"] is (scenario == "timeout-stopped-launcher"))
    if scenario in {"positive", "dirty-source"}:
        require((launcher == 0) is (scenario == "positive"))
        require(facts["nestedClientObserved"] is False and facts["launcherStopped"] is False)
    else:
        require(launcher != 0)
        require(facts["nestedClientObserved"] is True and facts["launcherStopped"] is True)
        if scenario == "timeout-stopped-launcher":
            require(elapsed >= 900 * 10**9)


def native_projection(
    data: bytes, status: int, scenario: str, expected_hashes: Mapping[str, str]
) -> dict[str, object]:
    """Require actual local system-tool/source joins in addition to the closed native case."""
    value = native_frame(data)
    require_native_case(value, status, scenario)
    facts = dictionary(value["facts"])
    require(
        set(expected_hashes) == {"opsSha256", "unshareSha256", "launcherSha256", "timeoutSha256"}
    )
    for name, checksum in expected_hashes.items():
        require(facts[name] == checksum)
    return value


def rendered_inputs(
    sources: Mapping[str, bytes], identity: Mapping[str, str], secret_values: Mapping[str, str]
) -> dict[str, bytes]:
    """Render original templates with a dotted private origin and coordinated synthetic secrets."""
    require(set(secret_values) == set(SECRET_NAMES))
    require(all(re.fullmatch(r"[a-f0-9]{64}", value) for value in secret_values.values()))
    require(set(identity) == {"image", "revision", "runId", "postgres", "caddy", "bridge"})
    _ = digest(identity["revision"], 40)
    _ = digest(identity["runId"], 32)
    require(re.fullmatch(r"sha256:[a-f0-9]{64}", identity["image"]))
    require(
        identity["caddy"] == caddy.IMAGE and re.fullmatch(r"sc[a-f0-9]{12}", identity["bridge"])
    )
    values: dict[str, object] = {
        "scpub_config": str(CONFIG),
        "scpub_root": str(PUBLIC),
        "scpub_server_image": identity["image"],
        "scpub_postgres_image": identity["postgres"],
        "scpub_caddy_image": identity["caddy"],
        "scpub_domain": NAME,
        "scpub_webauthn_rp_id": NAME,
        "scpub_announce_ip": "127.0.0.1",
        "scpub_announce_ipv6": "",
        "scpub_ipv6_network": "fd5c:5c68:a7d1::/64",
        "scpub_registration_enabled": False,
        "scpub_app_cpus": 1,
        "scpub_media_workers": 1,
        "scpub_app_memory_mib": 512,
        "scpub_max_connections": 175,
        "scpub_max_users": 100,
        "scpub_max_rooms": 175,
        "scpub_max_persisted_rooms": 100,
        "scpub_max_participants_per_room": 175,
        "scpub_max_broadcasters_per_room": 30,
        "scpub_postgres_memory_mib": 1024,
        "scpub_postgres_shared_buffers_mib": 256,
        "scpub_postgres_cpus": 1,
        "scpub_caddy_memory_mib": 256,
        "scpub_caddy_cpus": 0.5,
        "scpub_secrets": dict(secret_values),
    }
    environment = Environment(
        undefined=StrictUndefined,
        keep_trailing_newline=True,
        autoescape=False,  # noqa: S701 -- Render configuration bytes, never HTML.
    )
    outputs = {
        filename: environment.from_string(
            sources["ops/ansible/templates/public-" + template + ".j2"].decode()
        )
        .render(values)
        .encode()
        for filename, template in TEMPLATE_NAMES
    }
    base = object_value(json_value(cast("object", yaml.safe_load(sources["docker-compose.yml"]))))
    base_app = object_value(object_value(base["services"])["simplestchat"])
    base_app["ports"] = ["127.0.0.1:3000:3000", "127.0.0.1:40000:40000/udp"]
    object_value(base_app.setdefault("labels", {}))[LABEL] = identity["runId"]
    outputs["compose.base.yml"] = yaml.safe_dump(base, sort_keys=False).encode()
    public = object_value(json_value(cast("object", yaml.safe_load(outputs["compose.public.yml"]))))
    services = object_value(public["services"])
    for service in services.values():
        object_value(object_value(service).setdefault("labels", {}))[LABEL] = identity["runId"]
    proxy = object_value(services["caddy"])
    proxy["ports"] = ["127.0.0.1:80:80/tcp", "127.0.0.1:443:443/tcp", "127.0.0.1:443:443/udp"]
    public["networks"] = {"default": {"name": identity["bridge"], "external": True}}
    outputs["compose.public.yml"] = yaml.safe_dump(public, sort_keys=False).encode()
    outputs["Caddyfile"] = sources["Caddyfile"]
    outputs["secrets.json"] = encoded(dict(secret_values))
    outputs["images.json"] = encoded(
        {
            "revision": identity["revision"],
            "serverImage": identity["image"],
            "postgresImage": identity["postgres"],
            "caddyImage": identity["caddy"],
        }
    )
    outputs["browser-assets-installed.json"] = encoded(
        {"schemaVersion": 1, "caddyfileSha256": hashlib.sha256(sources["Caddyfile"]).hexdigest()}
    )
    return outputs


class NativeStatusError(ValueError):
    """Carry only the fixed current-call unexpected-status refusal."""


class NativeDeadlineError(ValueError):
    """Carry the original invocation end after a late observation or checked close."""


@final
class NativeCall:
    """Own exactly one installed-image invocation and both private evidence streams."""

    def __init__(self, control: Control, installed: stage.Original, wire: Wire) -> None:
        """Register passive current-call observations before any pipe or child effect."""
        self.control, self.installed, self.wire = control, installed, wire
        self.status: int | None = None
        self.output = b""
        self.error = b""
        self.output_complete = False
        self.pipes_closed = False
        self.sinks_closed = False
        self.called = False
        self.phase_end: float | None = None
        self.monitor: Callable[[], None] | None = None
        self.monitor_error: BaseException | None = None

    def check(self) -> None:
        """Fence the same caller invocation end as well as its original outer keeper."""
        self.control.check()
        if self.phase_end is not None and time.monotonic() >= self.phase_end:
            message = "maintain_prerequisite_native_deadline"
            raise NativeDeadlineError(message)

    def healthy(self) -> bool:
        """Observe the same owner and live collector; no nested daemon role is admitted here."""
        try:
            self.check()
            self.wire.no_extra()
            if self.monitor is not None:
                self.monitor()
        except (OSError, ValueError, RuntimeError, caddy.PrototypeError) as error:
            self.monitor_error = self.monitor_error or error
            return False
        return True

    def invoke(self, scenario: str) -> None:  # noqa: PLR0915 -- One current child owns all peers.
        """Use unchanged pump/installed FD; the original keeper ceiling is an outer backstop."""
        require(scenario in CASES and not self.called)
        self.called = True
        self.check()
        self.wire.no_extra()
        streams: list[BinaryIO] = []
        child: subprocess.Popen[bytes] | None = None
        consumed = False
        first: BaseException | None = None
        try:
            for suffix in ("stdout", "stderr"):
                self.check()
                descriptor = os.open(
                    PRIVATE / ("native." + suffix),
                    os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
                    PRIVATE_MODE,
                )
                streams.append(stage.adopt(descriptor, "w+b"))
                self.check()
            self.installed.check()
            self.check()
            require(not self.control.setup and self.control.body - time.monotonic() >= DEBT)
            origin = time.monotonic()
            end = min(origin + NATIVE, self.control.body - OBSERVATION)
            self.phase_end = end
            require(end - origin >= NATIVE)
            child = subprocess.Popen(  # noqa: S603 -- Retained executable and fixed role arguments.
                [
                    str(INSTALLED),
                    "release",
                    "maintain-prerequisite",
                    "run",
                    "--trial",
                    TRIALS[scenario],
                    "--timeout-seconds",
                    "1050",
                    "--finalization-seconds",
                    "60",
                ],
                executable="/proc/self/fd/" + str(self.installed.file.fileno()),
                pass_fds=(self.installed.file.fileno(),),
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
                cwd=ROOT,
                env=ENV,
            )
            self.check()
            require(end > time.monotonic())
            self.status = bounded_process.pump(
                child,
                streams[0],
                streams[1],
                bounded_process.Limits(
                    timeout=end - time.monotonic(),
                    stdout=MAX_RECORD,
                    stderr=MAX_STREAM,
                    healthy=self.healthy,
                ),
            )
            consumed = True
            if self.status != EXPECTED_STATUS[scenario]:
                message = "maintain_prerequisite_native_status"
                first = self.monitor_error or NativeStatusError(message)
            self.check()
            for stream in streams:
                self.check()
                stream.flush()
                self.check()
                _ = stream.seek(0)
                self.check()
            self.output = streams[0].read(MAX_RECORD + 1)
            self.check()
            self.error = streams[1].read(MAX_STREAM + 1)
            self.check()
            require(len(self.output) <= MAX_RECORD and len(self.error) <= MAX_STREAM)
            self.installed.check()
            self.check()
            self.output_complete = True
        except BaseException as error:  # noqa: BLE001 -- Settle all admitted peers.
            first = first or self.monitor_error or error
            if child is not None:
                if not consumed and child.returncode is None:
                    first = ops.settle((lambda: bounded_process.stop(child),), first)
                if child.returncode is not None:
                    self.status = child.returncode
        actions: list[Callable[[], None]] = []
        if child is not None:
            actions.extend(
                stream.close
                for stream in (child.stdin, child.stdout, child.stderr)
                if stream is not None
            )
        actions.extend(stream.close for stream in streams)
        first = ops.settle(actions, first)
        self.pipes_closed = child is not None and all(
            stream is None or stream.closed for stream in (child.stdin, child.stdout, child.stderr)
        )
        self.sinks_closed = len(streams) == PIPE_COUNT and all(stream.closed for stream in streams)
        first = ops.settle((self.check,), first)
        if first is not None:
            raise first
        require(self.pipes_closed and self.sinks_closed and self.output_complete)


@final
class LiveEvents:
    """Retain live bounded events without claiming that unobservable global drops are absent."""

    def __init__(self, commands: caddy.Commands, since: str) -> None:
        """Register every stream/child before fallible setup or a post-admission fence."""
        self.commands = commands
        self.child: subprocess.Popen[bytes] | None = None
        self.streams: list[BinaryIO] = []
        self.flags: list[tuple[BinaryIO, int]] = []
        self.rows: list[dict[str, object]] = []
        self.buffer = bytearray()
        self.counts = [0, 0]
        self.eof = [False, False]
        self.closed = False
        self.status: int | None = None
        self.marker: str | None = None
        self.native_start = 0
        self.failure: BaseException | None = None
        first: BaseException | None = None
        try:
            commands.control.check()
            require(re.fullmatch(r"[0-9]+\.[0-9]{9}", since))
            commands.tool_owners["docker"].check(commands.control)
            for suffix in ("stdout", "stderr"):
                commands.control.check()
                descriptor = os.open(
                    PRIVATE / ("daemon-events." + suffix),
                    os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
                    PRIVATE_MODE,
                )
                self.streams.append(stage.adopt(descriptor, "w+b"))
                commands.control.check()
            self.child = subprocess.Popen(  # noqa: S603 -- Exact retained event role.
                [
                    commands.tools["docker"],
                    "--host",
                    "unix:///var/run/docker.sock",
                    "events",
                    "--since",
                    since,
                    "--format",
                    "{{json .}}",
                ],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
                cwd=ROOT,
                env=ENV,
            )
            # The complete Popen object owns both existing pipe peers before either fcntl.
            commands.control.check()
            for stream in (self.child.stdout, self.child.stderr):
                require(stream is not None)
                pipe = cast("BinaryIO", required(stream))
                original = fcntl.fcntl(pipe.fileno(), fcntl.F_GETFL)
                self.flags.append((pipe, original))
                _ = fcntl.fcntl(pipe.fileno(), fcntl.F_SETFL, original | os.O_NONBLOCK)
                commands.control.check()
        except BaseException as error:  # noqa: BLE001 -- Settle all admitted peers.
            first = error
        if first is not None:
            failure = ops.settle((self.finish,), first)
            if failure is not None:
                raise failure from None

    def drain(self, *, final: bool = False) -> None:
        """Apply original fences to every physical read/write and keep all raw rows private."""
        self.commands.control.check()
        child = required(self.child)
        for index, stream in enumerate((child.stdout, child.stderr)):
            pipe = cast("BinaryIO", required(stream))
            while not self.eof[index]:
                self.commands.control.check()
                try:
                    value = os.read(pipe.fileno(), 65536)
                except (BlockingIOError, InterruptedError):
                    break
                self.commands.control.check()
                if not value:
                    self.eof[index] = True
                    break
                self.counts[index] += len(value)
                require(self.counts[index] <= MAX_STREAM)
                require(self.streams[index].write(value) == len(value))
                self.commands.control.check()
                if index == 0:
                    self.buffer.extend(value)
                    while b"\n" in self.buffer:
                        line, _, rest = self.buffer.partition(b"\n")
                        self.buffer = bytearray(rest)
                        require(len(self.rows) < MAX_EVENTS)
                        self.rows.append(record(bytes(line)))
                    require(len(self.buffer) <= MAX_RECORD)
        if not final:
            require(not any(self.eof) and not bounded_process.leader_exited(child.pid))

    def check(self) -> None:
        """Keep actual collector and wire guard live while a native call is quiet."""
        self.drain()
        if self.commands.watch is not None:
            self.commands.watch.check()

    def join_marker(self, identifier: str) -> None:
        """Require this known setup creation in the observed stream before admitting native GO."""
        _ = digest(identifier)
        end = time.monotonic() + self.commands.control.remaining(5)
        while True:
            self.check()
            require(time.monotonic() < end)
            if any(
                row.get("Type") == "network"
                and row.get("Action") == "create"
                and dictionary(row.get("Actor")).get("ID") == identifier
                for row in self.rows
            ):
                self.commands.control.check()
                require(time.monotonic() < end)
                self.marker = identifier
                return
            time.sleep(0.01)

    def native_boundary(self) -> None:
        """Join the captured prefix immediately before the one actual native admission."""
        require(self.marker is not None)
        self.check()
        self.native_start = len(self.rows)

    def retire(self, phase_check: Callable[[], None], end: float) -> None:
        """Retire the current reserved collector group inside its existing stop/drain/reap debt."""
        child = self.child
        if child is None or child.returncode is not None:
            return
        phase_check()
        if not bounded_process.leader_exited(child.pid):
            os.killpg(child.pid, signal.SIGINT)
        phase_check()
        drain_end = end - 6
        while time.monotonic() < drain_end:
            self.drain(final=True)
            phase_check()
            if all(self.eof) and bounded_process.leader_exited(child.pid):
                phase_check()
                break
            time.sleep(0.01)
        require(all(self.eof) and not self.buffer)
        require(bounded_process.wait_exited(child.pid, max(0.001, drain_end - time.monotonic())))
        phase_check()
        bounded_process.stop(child, grace=1)
        self.status = cast("int | None", child.returncode)
        phase_check()
        require(self.status in (0, -int(signal.SIGINT), 128 + int(signal.SIGINT)))

    def close_actions(self, phase_check: Callable[[], None]) -> list[Callable[[], None]]:
        """Enumerate every admitted pipe/sink and original flag independently of prior failure."""
        actions: list[Callable[[], None]] = []
        child = self.child
        if child is not None and child.returncode is None:

            def retire() -> None:
                bounded_process.stop(child, grace=1)
                phase_check()

            actions.append(retire)
        for stream, flags in self.flags:

            def restore(stream: BinaryIO = stream, flags: int = flags) -> None:
                _ = fcntl.fcntl(stream.fileno(), fcntl.F_SETFL, flags)
                phase_check()

            actions.append(restore)
        peers = list(self.streams)
        if child is not None:
            peers.extend(
                cast("BinaryIO", stream)
                for stream in (child.stdin, child.stdout, child.stderr)
                if stream is not None
            )
        for stream in peers:

            def close(stream: BinaryIO = stream) -> None:
                stream.close()
                phase_check()

            actions.append(close)
        return actions

    def finish(self) -> None:
        """Keep one saved15-second ceiling through terminal/EOF, retirement and all peer closes."""
        if self.closed:
            return
        self.closed = True
        first = self.failure
        end: float | None = None

        def phase_check() -> None:
            self.commands.control.check()
            if end is not None:
                require(time.monotonic() < end)

        try:
            phase_check()
            end = time.monotonic() + self.commands.control.remaining(15)
            self.retire(phase_check, end)
        except BaseException as error:  # noqa: BLE001 -- All known peers still settle.
            first = first or error
        first = ops.settle((*self.close_actions(phase_check), phase_check), first)
        if self.child is not None:
            self.status = self.child.returncode
        self.failure = first
        if first is not None:
            raise first


@final
class ConfigInputs:
    """Borrow existing Held checks for the fixed mixed-UID configuration originals."""

    def __init__(self, control: Control) -> None:
        """Retain the original parent and every admitted stream before any post-open refusal."""
        self.control, self.closed = control, False
        self.files: list[ops.Held] = []
        self.streams: list[BinaryIO] = []
        self.parent = -1
        self.before: os.stat_result | None = None
        control.check()
        self.parent = ops.directory(CONFIG)
        try:
            self.before = os.fstat(self.parent)
            for name, uid, mode in (
                ("app.env", 0, 0o600),
                ("migration.env", 0, 0o600),
                ("proxy.env", 0, 0o600),
                ("runtime-grants.sql", 0, 0o600),
                ("postgres-admin-password", 999, 0o600),
                ("pg_hba.conf", 999, 0o600),
                ("init-database.sql", 999, 0o600),
                ("Caddyfile", 0, 0o644),
                ("images.json", 0, 0o600),
                ("compose.base.yml", 0, 0o600),
                ("compose.public.yml", 0, 0o600),
                ("secrets.json", 0, 0o600),
                ("browser-assets-installed.json", 0, 0o600),
            ):
                control.check()
                fd = os.open(
                    name,
                    os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK,
                    dir_fd=self.parent,
                )
                stream = stage.adopt(fd, "rb")
                self.streams.append(stream)
                control.check()
                item = ops.Held(stream, name, os.fstat(stream.fileno()), "", 0)
                self.files.append(item)
                require(
                    stat.S_ISREG(item.metadata.st_mode)
                    and item.metadata.st_uid == item.metadata.st_gid == uid
                    and item.metadata.st_nlink == 1
                    and stat.S_IMODE(item.metadata.st_mode) == mode
                    and 0 < item.metadata.st_size <= MAX_RECORD
                )
                item.bytes = item.metadata.st_size
                item.sha256 = ops.digest_stream(stream, item.bytes, control.check)
            self.check()
        except BaseException as error:
            failure = ops.settle((self.close,), error)
            if failure is not None:
                raise failure from None
            raise

    def check(self) -> None:
        """Permit seed outputs while input names, bytes and parent binding stay fixed."""
        self.control.check()
        require(self.before is not None)
        before = required(self.before)
        require(
            ops.stamp(os.fstat(self.parent))[:5]
            == ops.stamp(before)[:5]
            == ops.stamp(CONFIG.lstat())[:5]
        )
        for item in self.files:
            item.check(self.parent, self.control.check)

    def close(self) -> None:
        """Attempt all known file and directory closes once after any refusal."""
        if self.closed:
            return
        self.closed = True
        actions: list[Callable[[], None]] = [stream.close for stream in self.streams]
        if self.parent >= 0:
            parent, self.parent = self.parent, -1
            actions.append(lambda: os.close(parent))
        failure = ops.settle(actions)
        if failure is not None:
            raise failure


def create_input(
    path: Path, data: bytes, control: Control, *, mode: int = 0o600, uid: int = 0
) -> None:
    """Create private first, complete/fsync, then apply one explicit fixture role profile."""
    control.check()
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    stream = stage.adopt(fd, "wb")
    first: BaseException | None = None
    try:
        control.check()
        for at in range(0, len(data), 65536):
            control.check()
            chunk = data[at : at + 65536]
            require(stream.write(chunk) == len(chunk))
            control.check()
        stream.flush()
        control.check()
        os.fchown(stream.fileno(), uid, uid)
        control.check()
        os.fchmod(stream.fileno(), mode)
        control.check()
        os.fsync(stream.fileno())
        control.check()
    except BaseException as error:  # noqa: BLE001 -- Settle all admitted peers.
        first = error
    first = ops.settle((stream.close, control.check), first)
    if first is not None:
        raise first


def actor_action(
    action: object, service: object, attrs: Mapping[str, object], phases: list[str]
) -> None:
    """Refuse every unreviewed actor action and bind expected TERM/stop to an active generation."""
    require(action in {"create", "start", "kill", "die", "stop", "destroy"})
    if action == "kill":
        require(service in {"migrate", "simplestchat"} and phases == ["create", "start"])
        require(attrs.get("signal") == "15")
    elif action == "stop":
        require(service in {"migrate", "simplestchat"} and phases == ["create", "start", "die"])


def actor_mutation(
    identifier: str, action: object, current: str | None, seen: set[tuple[str, str]]
) -> None:
    """Allow each fixed stop signal only once on the generation the launcher actually replaced."""
    if action in {"kill", "stop"}:
        key = (identifier, cast("str", action))
        require(identifier != current and key not in seen)
        seen.add(key)


def pg_exec_history(
    rows: Sequence[Mapping[str, object]], commands: Mapping[str, int], health: str
) -> None:
    """Join exact bounded per-exec create/start/die commands and canonical successful statuses."""
    executions: dict[str, tuple[str, list[str]]] = {}
    for row in rows:
        action = row.get("Action")
        require(type(action) is str)
        if action == "health_status: healthy":
            continue
        attributes = dictionary(dictionary(row.get("Actor")).get("Attributes"))
        identifier = digest(attributes.get("execID"))
        if cast("str", action).startswith("exec_create: "):
            command = cast("str", action).removeprefix("exec_create: ")
            require(identifier not in executions and command in {*commands, health})
            executions[identifier] = (command, ["create"])
            continue
        require(identifier in executions)
        command, phases = executions[identifier]
        if action == "exec_start: " + command:
            require(phases == ["create"])
            phases.append("start")
        else:
            require(action == "exec_die" and phases == ["create", "start"])
            require(attributes.get("exitCode") == "0")
            phases.append("die")
    require(all(phases == ["create", "start", "die"] for _, phases in executions.values()))
    for command, count in commands.items():
        require(sum(actual == command for actual, _ in executions.values()) == count)


@final
class Fixture:
    """Own real source/PG/Caddy setup, one native launch and independent bounded observations."""

    def __init__(self, value: Mapping[str, object], control: Control, wire: Wire) -> None:
        """Register known owners and passive observations before admitting any resource."""
        self.value, self.control, self.wire = value, control, wire
        self.originals: list[stage.Original] = []
        self.materials: list[caddy.Original] = []
        self.sources: dict[str, bytes] = {}
        self.commands: caddy.Commands | None = None
        self.collector: LiveEvents | None = None
        self.watch: caddy.Watch | None = None
        self.protected: ConfigInputs | None = None
        self.installed: stage.Original | None = None
        self.native: NativeCall | None = None
        self.native_record: dict[str, object] | None = None
        self.manifest: artifact.Manifest | None = None
        self.image = ""
        self.source_root = SOURCE_ROOT / digest(value["revision"], 40)
        self.network: str | None = None
        self.pg: str | None = None
        self.proxy: str | None = None
        self.app: str | None = None
        self.fixture_actors: set[str] = set()
        self.unknown_effect = False
        self.bridge = "sc" + digest(value["runId"], 32)[:12]
        self.table = "sc_maintain_" + digest(value["runId"], 32)[:12]
        self.guarded = False
        self.guard_facts: dict[str, object] = {}
        self.caddy_facts: dict[str, JsonValue] = {}
        self.report: dict[str, object] = {}
        self.observed: dict[str, object] = dict.fromkeys(OBSERVATION_FIELDS, False)
        self.caddy_image = ""
        self.pg_image = ""
        self.pg_health = ""
        self.attempts_before: set[str] = set()
        self.baseline_counts = b""
        self.socket: tuple[Path, os.stat_result] | None = None
        self.finished = False
        self.native_rows: list[dict[str, object]] = []

    def retain(self, path: Path, maximum: int = MAX_RECORD, mode: int = 0o600) -> stage.Original:
        """Keep the exact admitted original through all native and local finalizers."""
        original = stage.Original(
            path, maximum, mode, cast("stage.Control", cast("object", self.control))
        )
        self.originals.append(original)
        return original

    def originals_check(self) -> None:
        """Check complete original bytes and the actual original daemon socket namespace."""
        for original in self.originals:
            original.check()
        if self.protected is not None:
            self.protected.check()
        for original in self.materials:
            original.check(cast("caddy.Control", cast("object", self.control)))
        if self.socket is not None:
            path, before = self.socket
            self.control.check()
            require(Path("/var/run/docker.sock").resolve(strict=True) == path)
            require(ops.stamp(path.lstat())[:5] == ops.stamp(before)[:5])

    def call(self, role: str, arguments: Sequence[str], cap: float = 30) -> bytes:
        """Borrow only exact retained tools and the same live original controller."""
        self.control.check()
        self.wire.no_extra()
        self.originals_check()
        return required(self.commands).call(role, arguments, cap)

    def docker(self, role: str, arguments: Sequence[str], cap: float = 15) -> bytes:
        """Invoke only the fixed local Docker socket and bounded role arguments."""
        return self.call(role, ["docker", "--host", "unix:///var/run/docker.sock", *arguments], cap)

    def compose(self, role: str, arguments: Sequence[str], cap: float = 30) -> bytes:
        """Use the actual unchanged fixed public Compose wrapper."""
        return self.call(role, ["public", *arguments], cap)

    def source_setup(self) -> None:
        """Import only the exact one-commit shallow bundle, without claiming complete history."""
        self.control.checkpoint = "source-import"
        require(os.geteuid() == os.getegid() == 0 and sys.platform == "linux")
        marker = self.retain(Path("/etc/simplestchat-vm-fixture"), 128)
        require(marker.data() == (cast("str", self.value["runId"]) + "\n").encode())
        self.control.check()
        bundle = self.retain(BUNDLE, MAX_BUNDLE)
        bundle_pin = dictionary(self.value["sourceBundle"])
        require(
            bundle.sha256 == bundle_pin["sha256"] and bundle.before.st_size == bundle_pin["bytes"]
        )
        self.commands = caddy.Commands(PRIVATE, cast("caddy.Control", cast("object", self.control)))
        for name in ("docker", "git", "nft", "tcpdump", "ip", "curl"):
            owner = caddy.Tool.open(name, cast("caddy.Control", cast("object", self.control)))
            self.commands.tool_owners[name] = owner
            self.commands.tools[name] = str(owner.path)
        require(not list(self.docker("empty-daemon", ["ps", "--all", "--quiet"]).splitlines()))
        require(
            not self.docker(
                "empty-project-networks",
                [
                    "network",
                    "ls",
                    "--quiet",
                    "--filter",
                    "label=com.docker.compose.project=" + PROJECT,
                ],
            ).strip()
        )
        self.control.check()
        self.source_root.mkdir(mode=0o700)
        _ = self.call("source-init", ["git", "init", "--quiet", str(self.source_root)])
        create_input(
            self.source_root / ".git/shallow",
            (cast("str", self.value["revision"]) + "\n").encode(),
            self.control,
        )
        _ = self.call(
            "source-fetch",
            [
                "git",
                "-C",
                str(self.source_root),
                "fetch",
                "--quiet",
                "--no-tags",
                "--no-write-fetch-head",
                str(BUNDLE),
                "HEAD:refs/remotes/source/main",
            ],
        )
        _ = self.call(
            "source-checkout",
            [
                "git",
                "-C",
                str(self.source_root),
                "checkout",
                "--quiet",
                "--detach",
                cast("str", self.value["revision"]),
            ],
        )
        require(
            self.call(
                "source-head", ["git", "-C", str(self.source_root), "rev-parse", "HEAD"]
            ).strip()
            == cast("str", self.value["revision"]).encode()
        )
        require(
            self.call(
                "source-tree", ["git", "-C", str(self.source_root), "rev-parse", "HEAD^{tree}"]
            ).strip()
            == cast("str", self.value["sourceTree"]).encode()
        )
        require(
            self.call(
                "source-count", ["git", "-C", str(self.source_root), "rev-list", "--count", "HEAD"]
            ).strip()
            == b"1"
        )
        _ = self.call(
            "source-fsck", ["git", "-C", str(self.source_root), "fsck", "--connectivity-only"]
        )
        require(
            not self.call(
                "source-clean",
                [
                    "git",
                    "-C",
                    str(self.source_root),
                    "status",
                    "--porcelain",
                    "--untracked-files=all",
                ],
            ).strip()
        )
        pins = exact(self.value["sourcePins"], set(GUEST_SOURCES))
        for name, checksum in pins.items():
            path = ROOT / Path(name).name if name in STAGED_SOURCES else self.source_root / name
            mode = 0o700 if name == "ops/ansible/files/seed-public-room.py" else 0o600
            original = self.retain(path, 2**20, mode)
            require(original.sha256 == checksum)
            self.sources[name] = original.data(2**20)
        helpers = exact(
            self.value["helperPins"],
            set(ops.BASE | ops.FETCH | ops.FORCE) | {"security_vm_ops_guest.py"},
        )
        for name, checksum in helpers.items():
            original = self.retain(
                ROOT / name if name == "security_vm_ops_guest.py" else LIBRARY / name,
                2**20,
                0o600 if name == "security_vm_ops_guest.py" else 0o644,
            )
            require(original.sha256 == checksum)
        self.installed = self.retain(INSTALLED, ops.MAX_BINARY, 0o755)
        binary = dictionary(self.value["binary"])
        require(
            self.installed.sha256 == binary["sha256"]
            and self.installed.before.st_size == binary["bytes"]
        )
        self.control.check()
        socket_path = Path("/var/run/docker.sock").resolve(strict=True)
        before = socket_path.lstat()
        require(stat.S_ISSOCK(before.st_mode) and before.st_uid == 0)
        self.socket = socket_path, before
        self.native = NativeCall(self.control, self.installed, self.wire)

    def payload_and_inputs(self) -> None:
        """Load the authenticated archive graph and render the real source profile."""
        self.control.checkpoint = "payload"
        for name, key, maximum in (
            ("release.json", "manifest", MAX_RECORD),
            ("image.tar", "archive", 2 * 1024**3),
        ):
            original = self.retain(PAYLOAD_ROOT / name, maximum)
            pin = dictionary(self.value[key])
            require(original.sha256 == pin["sha256"] and original.before.st_size == pin["bytes"])
        self.control.check()
        self.manifest = artifact.validate_manifest(PAYLOAD_ROOT / "release.json")
        require(self.manifest["revision"] == self.value["revision"])
        identities, config = stage.archive_graph(PAYLOAD_ROOT / "image.tar", self.manifest)
        self.originals_check()
        _ = self.docker(
            "server-load", ["image", "load", "--input", str(PAYLOAD_ROOT / "image.tar")], 120
        )
        observed = self.docker("server-image", ["image", "inspect", self.manifest["imageTag"]])
        self.image = stage.image_profile(
            decode_json(observed), identities, config, cast("str", self.value["revision"])
        )
        for role, selected in (
            ("postgres", self.value["postgresImage"]),
            ("caddy", self.value["caddyImage"]),
        ):
            _ = self.docker(role + "-pull", ["pull", cast("str", selected)], 90)
            row = caddy.one_inspection(
                self.docker(role + "-image", ["image", "inspect", cast("str", selected)])
            )
            require(row["Os"] == "linux" and row["Architecture"] == "amd64")
            require(
                any(
                    cast("str", value).endswith("@" + cast("str", selected).split("@", 1)[1])
                    for value in cast("list[object]", row["RepoDigests"])
                )
            )
            if role == "caddy":
                self.caddy_image = digest(cast("str", row["Id"]).removeprefix("sha256:"))
                self.caddy_image = "sha256:" + self.caddy_image
            else:
                self.pg_image = "sha256:" + digest(cast("str", row["Id"]).removeprefix("sha256:"))
        self.control.checkpoint = "render"
        values = {name: secrets.token_hex(32) for name in SECRET_NAMES}
        outputs = rendered_inputs(
            self.sources,
            {
                "image": self.image,
                "revision": cast("str", self.value["revision"]),
                "runId": cast("str", self.value["runId"]),
                "postgres": cast("str", self.value["postgresImage"]),
                "caddy": cast("str", self.value["caddyImage"]),
                "bridge": self.bridge,
            },
            values,
        )
        for name, data in outputs.items():
            create_input(
                CONFIG / name,
                data,
                self.control,
                uid=999
                if name in {"postgres-admin-password", "pg_hba.conf", "init-database.sql"}
                else 0,
                mode=0o644 if name == "Caddyfile" else 0o600,
            )
        for target, (source, mode) in SOURCE_ROLES.items():
            path = Path(target)
            if path.is_relative_to(LIBRARY) and not absent(path, self.control):
                original = self.retain(path, 2**20, mode)
                require(original.data(2**20) == self.sources[source])
            else:
                create_input(path, self.sources[source], self.control, mode=mode)
                _ = self.retain(path, 2**20, mode)
        for name, uid, mode in (
            ("postgres", 999, 0o700),
            ("postgres-socket", 999, 0o755),
            ("caddy-data", 10001, 0o700),
            ("caddy-config", 10001, 0o700),
        ):
            self.control.check()
            path = PUBLIC / name
            path.mkdir(mode=0o700)
            os.chown(path, uid, uid)
            path.chmod(mode)
            self.control.check()
        self.control.check()
        _ = release_assets.initialize(PUBLIC / "browser-assets")
        self.control.check()
        self.protected = ConfigInputs(self.control)
        commands = required(self.commands)
        for role, executable in (
            ("public", "/usr/local/bin/simplestchat-public"),
            ("unshare", "/usr/bin/unshare"),
            ("timeout", "/usr/bin/timeout"),
        ):
            owner = caddy.Tool.open(executable, cast("caddy.Control", cast("object", self.control)))
            commands.tools[role], commands.tool_owners[role] = str(owner.path), owner
            require(
                stat.S_IMODE(owner.stamp.st_mode) == EXECUTABLE_MODE and owner.stamp.st_nlink == 1
            )
        self.originals_check()
        _ = self.compose("compose-admission", ["--profile", "maintenance", "config", "--quiet"])
        require(
            self.docker(
                "server-id-check", ["image", "inspect", "--format", "{{.Id}}", self.image]
            ).strip()
            == self.image.encode()
        )

    def setup_network(self) -> None:
        """Start the live collector before joining one known setup-network marker."""
        commands = required(self.commands)
        now = time.time_ns()
        since = str(now // 10**9) + "." + format(now % 10**9, "09d")
        self.collector = LiveEvents(commands, since)
        self.unknown_effect = True

        def created(data: bytes) -> None:
            require(re.fullmatch(rb"[a-f0-9]{64}\n", data))
            self.network = data.strip().decode("ascii")
            self.unknown_effect = False

        _ = commands.call(
            "network-create",
            [
                "docker",
                "--host",
                "unix:///var/run/docker.sock",
                "network",
                "create",
                "--internal",
                "--driver",
                "bridge",
                "--opt",
                "com.docker.network.bridge.name=" + self.bridge,
                "--label",
                LABEL + "=" + cast("str", self.value["runId"]),
                self.bridge,
            ],
            adopt=created,
        )
        required(self.collector).join_marker(required(self.network))
        bridge = caddy.Bridge(required(self.network), self.bridge, self.caddy_image)
        caddy.bridge_identity(
            bridge,
            caddy.one_inspection(
                self.docker("network-profile", ["network", "inspect", required(self.network)])
            ),
        )
        routes = cast(
            "list[object]",
            decode_json(self.call("physical-route", ["ip", "-json", "route", "show", "default"])),
        )
        require(type(routes) is list and len(routes) == 1)
        interface = dictionary(routes[0])["dev"]
        require(type(interface) is str and re.fullmatch(r"[A-Za-z0-9_.-]{1,15}", interface))
        rules = caddy.firewall(self.table, cast("str", interface), self.bridge, "10.0.2.2")
        create_input(PRIVATE / "guard.nft", rules.encode(), self.control)

        def guarded(_data: bytes) -> None:
            self.guarded = True

        _ = commands.call("guard-install", ["nft", "-f", str(PRIVATE / "guard.nft")], adopt=guarded)
        self.watch = caddy.Watch.start(commands, cast("str", interface), "10.0.2.2")
        commands.watch = self.watch
        caddy.guard_probe(commands, self.table)
        self.observed["collectorMarkerJoined"] = True

    def service(self, name: str, image: str) -> tuple[str, dict[str, object]]:
        """Join a completed service effect to its current CID, image and nonce."""
        require(name in {"postgres", "simplestchat", "caddy"})
        raw = self.compose("service-" + name, ["ps", "--quiet", name]).strip()
        require(re.fullmatch(rb"[a-f0-9]{64}", raw))
        identifier = raw.decode("ascii")
        row = caddy.one_inspection(self.docker("inspect-" + name, ["inspect", identifier]))
        labels = dictionary(dictionary(row["Config"])["Labels"])
        require(row["Id"] == identifier and row["Image"] == image)
        require(
            labels.get(LABEL) == self.value["runId"]
            and labels.get("com.docker.compose.project") == PROJECT
            and labels.get("com.docker.compose.service") == name
        )
        require(dictionary(row["State"])["Running"] is True)
        self.fixture_actors.add(identifier)
        if name == "caddy" and identifier not in required(self.commands).owned:
            required(self.commands).owned.append(identifier)
        return identifier, row

    def sql(self, role: str, query: str) -> bytes:
        """Observe only fixed administration queries against the exact joined source PG actor."""
        return self.docker(
            role,
            [
                "exec",
                "--user",
                "999:999",
                required(self.pg),
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
                "--command",
                query,
            ],
            15,
        )

    def setup_database(self) -> None:
        """Initialize empty PostgreSQL roles without migrating or seeding ahead of the launcher."""
        self.control.checkpoint = "database-setup"
        self.unknown_effect = True
        _ = self.compose(
            "postgres-up",
            [
                "up",
                "--detach",
                "--no-build",
                "--pull",
                "never",
                "--wait",
                "--wait-timeout",
                "120",
                "postgres",
            ],
            135,
        )
        self.pg, row = self.service("postgres", self.pg_image)
        self.unknown_effect = False
        host = dictionary(row["HostConfig"])
        require(
            host["NetworkMode"] == "none"
            and host["ReadonlyRootfs"] is True
            and host["CapDrop"] == ["ALL"]
        )
        config = dictionary(row["Config"])
        require(config["User"] == "999:999")
        health = dictionary(config["Healthcheck"])["Test"]
        require(type(health) is list and len(cast("list[object]", health)) == PIPE_COUNT)
        parts = cast("list[object]", health)
        require(parts[0] == "CMD-SHELL" and type(parts[1]) is str)
        self.pg_health = "/bin/sh -c " + cast("str", parts[1])
        require(
            self.sql(
                "empty-database",
                (
                    "SELECT to_regclass('public._sqlx_migrations') IS NULL "
                    + "AND to_regclass('public.users') IS NULL AND "
                    + "to_regclass('public.rooms') IS NULL"
                ),
            ).strip()
            == b"t"
        )
        self.baseline_counts = self.sql(
            "initial-roles",
            (
                "SELECT rolname,NOT rolsuper AND NOT rolcreatedb AND NOT "
                + "rolcreaterole AND NOT rolreplication AND NOT rolbypassrls "
                + "FROM pg_roles WHERE rolname IN "
                + "('simplestchat_app','simplestchat_migrate') ORDER BY rolname"
            ),
        )
        require(self.baseline_counts == b"simplestchat_app|t\nsimplestchat_migrate|t\n")

    def proxy_profile(self, identifier: str) -> dict[str, object]:
        """Check the normal operational split-path mounts and restrictions without rewriting TLS."""
        row = caddy.one_inspection(self.docker("proxy-profile", ["inspect", identifier]))
        host, config = dictionary(row["HostConfig"]), dictionary(row["Config"])
        require(
            row["Id"] == identifier
            and row["Image"] == self.caddy_image
            and config["User"] == "10001:10001"
            and config["Image"] == caddy.IMAGE
        )
        require(
            host["ReadonlyRootfs"] is True
            and host["CapDrop"] == ["ALL"]
            and host["CapAdd"] == ["NET_BIND_SERVICE"]
            and host["Init"] is True
        )
        require("no-new-privileges" in cast("list[object]", host["SecurityOpt"]))
        require(
            host["Memory"] == 256 * 1024**2
            and host["PidsLimit"] == CADDY_PIDS
            and host["NanoCpus"] == CADDY_NANO_CPUS
        )
        expected = {
            "/etc/caddy/Caddyfile": (str(CONFIG / "Caddyfile"), False),
            "/data": (str(PUBLIC / "caddy-data"), True),
            "/config": (str(PUBLIC / "caddy-config"), True),
            "/run/simplestchat-browser-assets": (str(PUBLIC / "browser-assets"), False),
        }
        mounts = {
            cast("str", dictionary(item)["Destination"]): (
                dictionary(item)["Source"],
                dictionary(item)["RW"],
            )
            for item in cast("list[object]", row["Mounts"])
        }
        require(
            mounts == expected
            and all(
                dictionary(item)["Type"] == "bind" for item in cast("list[object]", row["Mounts"])
            )
        )
        require(
            host["NetworkMode"] == self.bridge
            and dictionary(host["RestartPolicy"])
            == {"Name": "unless-stopped", "MaximumRetryCount": 0}
        )
        require(
            dictionary(host["PortBindings"])
            == {
                "80/tcp": [{"HostIp": "127.0.0.1", "HostPort": "80"}],
                "443/tcp": [{"HostIp": "127.0.0.1", "HostPort": "443"}],
                "443/udp": [{"HostIp": "127.0.0.1", "HostPort": "443"}],
            }
        )
        return row

    def endpoint(self, identifier: str, peers: frozenset[str]) -> CaddyTarget:
        """Join every actual known bridge member; never silently drop an unknown network peer."""
        require(
            identifier in peers
            and peers <= (self.fixture_actors | set(required(self.commands).owned))
        )
        row = self.proxy_profile(identifier)
        network = caddy.one_inspection(
            self.docker("proxy-network", ["network", "inspect", required(self.network)])
        )
        bridge = caddy.Bridge(required(self.network), self.bridge, self.caddy_image)
        caddy.bridge_identity(bridge, network)
        members = dictionary(dictionary(row["NetworkSettings"])["Networks"])
        require(set(members) == {self.bridge})
        member = dictionary(members[self.bridge])
        require(member["NetworkID"] == self.network)
        address, prefix, subnet, gateway = caddy.bridge_addresses(member, network)
        actual = dictionary(network["Containers"])
        require(set(actual) == set(peers))
        node = dictionary(actual[identifier])
        require(
            node["EndpointID"] == member["EndpointID"]
            and node["IPv4Address"] == f"{address}/{prefix}"
        )
        state = dictionary(row["State"])
        require(
            state["Running"] is True
            and state["Restarting"] is False
            and state["Paused"] is False
            and state["Dead"] is False
            and state["OOMKilled"] is False
            and state["Status"] == "running"
        )
        _ = integer(state["Pid"], 1, 2**31 - 1)
        return CaddyTarget(
            self,
            identifier,
            peers,
            address,
            prefix,
            cast("str", member["EndpointID"]),
            subnet,
            gateway,
        )

    def tls(self, identifier: str, peers: frozenset[str]) -> dict[str, JsonValue]:
        """Keep exact SNI/Host/public-root trust and actual chain checks on the joined bridge IP."""
        target = self.endpoint(identifier, peers)
        context = ssl.create_default_context(cadata=self.materials[0].data.decode("ascii"))
        context.minimum_version = ssl.TLSVersion.TLSv1_2
        metadata = {"tlsCheckpoint": None, "tlsFailureCategory": None, "postTlsCheckpoint": None}
        return caddy.wait_tls(
            required(self.commands),
            context,
            cast("caddy.Endpoint", cast("object", target)),
            cast("caddy.Report", cast("object", metadata)),
        )

    def prepare_caddy(self) -> None:
        """Generate seven natural materials, then retire only the known setup CID."""
        self.control.checkpoint = "caddy-natural"
        commands = required(self.commands)
        arguments = caddy.normal_arguments(
            CONFIG, PUBLIC / "caddy-data", PUBLIC / "caddy-config", self.bridge
        )
        old = (
            f"type=bind,src={CONFIG / 'browser-assets'},"
            "dst=/run/simplestchat-browser-assets,readonly"
        )
        require(arguments.count(old) == 1)
        arguments[arguments.index(old)] = (
            f"type=bind,src={PUBLIC / 'browser-assets'},"
            "dst=/run/simplestchat-browser-assets,readonly"
        )
        arguments[:0] = ["--label", LABEL + "=" + cast("str", self.value["runId"])]
        identifier = commands.create("natural-caddy-create", arguments)
        self.fixture_actors.add(identifier)
        _ = self.proxy_profile(identifier)
        _ = self.docker("natural-caddy-start", ["start", identifier])
        end = time.monotonic() + self.control.remaining(30)
        root = PUBLIC / "caddy-data/caddy/pki/authorities/local/root.crt"
        while absent(root, self.control):
            require(time.monotonic() < end)
            required(self.watch).check()
            required(self.collector).check()
            time.sleep(0.05)
        self.control.check()
        require(time.monotonic() < end)
        self.materials.append(
            caddy.Original.open(root, cast("caddy.Control", cast("object", self.control)))
        )
        self.control.check()
        require(time.monotonic() < end)
        # A root can precede asynchronous leaf issuance. The real handshake triggers it.
        observed = self.tls(identifier, frozenset({identifier}))
        for relative in caddy_materials.PATHS[1:]:
            self.materials.append(
                caddy.Original.open(
                    PUBLIC / "caddy-data/caddy" / relative,
                    cast("caddy.Control", cast("object", self.control)),
                )
            )
        self.caddy_facts = caddy_materials.inspect(
            dict(zip(caddy_materials.PATHS, (item.data for item in self.materials), strict=True)),
            datetime.now(UTC),
            self.control.end - time.monotonic(),
        )
        caddy.bind_live_chain(observed, self.caddy_facts)
        _ = self.docker("natural-caddy-stop", ["stop", "--time", "5", identifier], 10)
        commands.remove(identifier)
        self.fixture_actors.remove(identifier)
        self.observed["naturalTlsPrepared"] = True
        self.originals_check()

    def prepare(self) -> None:
        """Finish all real setup under510 before any full1170 native admission."""
        self.source_setup()
        self.payload_and_inputs()
        self.setup_network()
        self.setup_database()
        self.prepare_caddy()
        _ = self.compose(
            "final-compose-admission", ["--profile", "maintenance", "config", "--quiet"]
        )
        self.originals_check()
        self.attempts_before = {path.name for path in (PUBLIC / "results").iterdir()}
        require(
            not self.attempts_before
            and absent(PUBLIC / "release-state.json", self.control)
            and absent(WORK / "current.json", self.control)
        )
        self.observed["sourceCommitMatched"] = True
        self.observed["sourceShallowBoundaryVerified"] = True

    def dirty_readme(self) -> None:
        """Change only the fixed nonexecuted README while keeping all execution helpers exact."""
        path = self.source_root / "README.md"
        self.control.check()
        before = path.lstat()
        require(
            stat.S_ISREG(before.st_mode)
            and before.st_uid == before.st_gid == 0
            and stat.S_IMODE(before.st_mode) == PRIVATE_MODE
            and before.st_nlink == 1
        )
        fd = os.open(path, os.O_WRONLY | os.O_APPEND | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
        first: BaseException | None = None
        try:
            self.control.check()
            require(ops.stamp(os.fstat(fd)) == ops.stamp(before))
            data = b"\nOwned prerequisite dirty-source fixture.\n"
            require(os.write(fd, data) == len(data))
            self.control.check()
            os.fsync(fd)
            self.control.check()
        except BaseException as error:  # noqa: BLE001 -- Settle all admitted peers.
            first = error
        first = ops.settle((lambda: os.close(fd), self.control.check), first)
        if first is not None:
            raise first
        require(
            self.call(
                "source-deliberately-dirty",
                [
                    "git",
                    "-C",
                    str(self.source_root),
                    "status",
                    "--porcelain",
                    "--untracked-files=all",
                ],
            )
            == b" M README.md\n"
        )
        self.observed["dirtyReadmeObserved"] = True

    def invoke(self) -> None:
        """Run the one attested installed image only after both original1170 admissions."""
        scenario = cast("str", self.value["scenario"])
        if scenario == "dirty-source":
            self.dirty_readme()
        self.originals_check()
        self.control.checkpoint = "ready"
        self.wire.emit(
            {
                "schemaVersion": 1,
                "scope": "MaintainLauncherPrerequisiteReady",
                "runId": self.value["runId"],
                "ready": True,
            }
        )
        go = exact(record(self.wire.line()), {"schemaVersion", "runId", "go"})
        require(
            type(go["schemaVersion"]) is int
            and go["schemaVersion"] == 1
            and go["runId"] == self.value["runId"]
            and go["go"] is True
        )
        self.wire.no_extra()
        self.control.advance()
        self.control.checkpoint = "native"
        collector = required(self.collector)
        collector.native_boundary()
        native = required(self.native)
        native.monitor = collector.check
        self.originals_check()
        self.control.check()
        require(self.control.body - time.monotonic() >= DEBT)
        self.unknown_effect = True
        native.invoke(scenario)
        self.control.checkpoint = "native-result"
        collector.check()
        self.native_rows = list(collector.rows[collector.native_start :])
        self.native_record = native_frame(native.output)
        commands = required(self.commands)
        expected = {
            "opsSha256": required(self.installed).sha256,
            "launcherSha256": hashlib.sha256(
                self.sources["ops/ansible/files/deploy-public.sh"]
            ).hexdigest(),
            "unshareSha256": commands.tool_owners["unshare"].sha256,
            "timeoutSha256": commands.tool_owners["timeout"].sha256,
        }
        _ = native_projection(native.output, required(native.status), scenario, expected)
        self.observed["systemToolHashesJoined"] = True

    def attempt(self) -> Path | None:
        """Retain the one new script attempt expected for this fixed unchanged source path."""
        self.control.check()
        names = {path.name for path in (PUBLIC / "results").iterdir()}
        added = names - self.attempts_before
        scenario = self.value["scenario"]
        if scenario in {"cancel-owned-launcher", "timeout-stopped-launcher"}:
            require(not added)
            return None
        require(len(added) == 1)
        name = next(iter(added))
        require(re.fullmatch(r"deploy\.[A-Za-z0-9]{8}", name))
        path = PUBLIC / "results" / name
        info = path.lstat()
        require(
            stat.S_ISDIR(info.st_mode)
            and info.st_uid == info.st_gid == 0
            and stat.S_IMODE(info.st_mode) == DIRECTORY_MODE
        )
        outcome = self.retain(path / "outcome.json")
        row = exact(
            record(outcome.data()),
            {"startedAt", "finishedAt", "phase", "exitStatus", "passed", "serverImage"},
        )
        positive = scenario == "positive"
        require(
            row["serverImage"] == self.image
            and type(row["exitStatus"]) is int
            and row["exitStatus"] == (0 if positive else 1)
            and row["passed"] is positive
            and row["phase"] == ("complete" if positive else "configuration")
        )
        for key in ("startedAt", "finishedAt"):
            require(
                type(row[key]) is str
                and re.fullmatch(
                    r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z",
                    cast("str", row[key]),
                )
            )
        digest_file = self.retain(path / "deployment-inputs.sha256")
        ordered = [
            "/usr/local/bin/simplestchat-public-deploy",
            "/usr/local/bin/simplestchat-public",
            str(CONFIG / "compose.base.yml"),
            str(CONFIG / "compose.public.yml"),
            str(CONFIG / "Caddyfile"),
        ]
        expected: list[bytes] = []
        for target in ordered:
            if target in SOURCE_ROLES:
                checksum = hashlib.sha256(self.sources[SOURCE_ROLES[target][0]]).hexdigest()
            else:
                item = next(
                    item
                    for item in required(self.protected).files
                    if CONFIG / item.name == Path(target)
                )
                checksum = item.sha256
            expected.append((checksum + "  " + target).encode())
        require(digest_file.data().splitlines() == expected)
        return path

    def expected_pg_commands(self) -> dict[str, int]:
        """Bind the finite unchanged-launcher exec catalog; other command effects are unproven."""
        if self.value["scenario"] != "positive":
            return {}
        source = self.sources["ops/ansible/files/deploy-public.sh"].decode("utf-8")
        queries = re.findall(r'--command "([^"\n]+)"', source)
        require(len(queries) == PIPE_COUNT)
        base = (
            "psql --no-psqlrc --set ON_ERROR_STOP=on --host /run/simplestchat-postgres "
            + "--username postgres --dbname simplestchat"
        )
        return {
            "id -un": 1,
            base + " --tuples-only --no-align --field-separator   --command " + queries[0]: 1,
            base: 1,
            base + " --tuples-only --no-align --command " + queries[1]: 1,
            "pg_dump --host /run/simplestchat-postgres --username postgres "
            + "--dbname simplestchat --format custom": 1,
        }

    def expected_actor_traces(
        self, validation: str | None, traces: Mapping[str, list[str]], services: Mapping[str, str]
    ) -> None:
        """Require exact expected validator, migration and application actor generations."""
        require(
            validation is not None
            and traces.get(validation) == ["create", "start", "die", "destroy"]
        )
        migration = [identifier for identifier, service in services.items() if service == "migrate"]
        apps = [identifier for identifier, service in services.items() if service == "simplestchat"]
        proxies = [identifier for identifier, service in services.items() if service == "caddy"]
        require(
            len(migration) == 1
            and traces.get(migration[0]) == ["create", "start", "die", "destroy"]
        )
        require(len(apps) == POSITIVE_APP_ACTORS and required(self.app) in apps)
        require(traces.get(required(self.app)) == ["create", "start"])
        require(
            all(
                traces.get(identifier) == ["create", "start", "die", "destroy"]
                for identifier in apps
                if identifier != self.app
            )
        )
        require(proxies == [self.proxy] and traces.get(required(self.proxy)) == ["create", "start"])

    def daemon_observe(self, attempt: Path | None) -> None:
        """Join expected actors and inventory without claiming globally complete history."""
        self.control.checkpoint = "daemon-observe"
        positive = self.value["scenario"] == "positive"
        if positive:
            self.app, _ = self.service("simplestchat", self.image)
            self.proxy, _ = self.service("caddy", self.caddy_image)
        current = {
            line.decode("ascii")
            for line in self.docker(
                "fresh-actor-inventory", ["ps", "--all", "--quiet", "--no-trunc"]
            ).splitlines()
        }
        expected = {
            required(self.pg),
            *([required(self.app), required(self.proxy)] if positive else []),
        }
        require(current == expected)
        self.observed["freshActorInventoryJoined"] = True
        traces: dict[str, list[str]] = {}
        services: dict[str, str] = {}
        validation: str | None = None
        mutations: set[tuple[str, str]] = set()
        for row in self.native_rows:
            kind, action = row.get("Type"), row.get("Action")
            require(type(action) is str)
            actor = dictionary(row.get("Actor"))
            attrs = dictionary(actor.get("Attributes"))
            identifier = actor.get("ID")
            if kind == "container":
                identifier = digest(identifier)
                if identifier == self.pg:
                    continue
                require(positive)
                service = attrs.get("com.docker.compose.service")
                if service in {"simplestchat", "migrate", "caddy"}:
                    require(
                        attrs.get(LABEL) == self.value["runId"]
                        and attrs.get("com.docker.compose.project") == PROJECT
                    )
                    require(
                        attrs.get("image")
                        == (self.caddy_image if service == "caddy" else self.image)
                        or attrs.get("image") == (caddy.IMAGE if service == "caddy" else self.image)
                    )
                    services[identifier] = cast("str", service)
                else:
                    require(attempt is not None)
                    expected_name = (
                        self.retain(required(attempt) / "validation-name.txt")
                        .data()
                        .strip()
                        .decode()
                    )
                    require(
                        attrs.get("name") == expected_name
                        and attrs.get("image") in {caddy.IMAGE, self.caddy_image}
                    )
                    validation = identifier
                actor_action(action, service, attrs, traces.setdefault(identifier, []))
                actor_mutation(identifier, action, self.app, mutations)
                if action in {"create", "start", "die", "destroy"}:
                    traces.setdefault(identifier, []).append(cast("str", action))
                    if action == "die" and (service == "migrate" or identifier == validation):
                        require(attrs.get("exitCode") == "0")
            elif kind == "network":
                require(
                    positive and identifier == self.network and action in {"connect", "disconnect"}
                )
            else:
                require(None)
        pg_exec_history(
            [
                row
                for row in self.native_rows
                if row.get("Type") == "container"
                and dictionary(row.get("Actor")).get("ID") == self.pg
            ],
            self.expected_pg_commands(),
            self.pg_health,
        )
        if positive:
            self.expected_actor_traces(validation, traces, services)
        self.observed["observedExpectedDaemonJoins"] = True
        self.observed["noObservedUnexpectedMutatingEvents"] = True
        self.unknown_effect = False

    def database_observe(self, attempt: Path | None) -> None:
        """Observe only fixed ledger/count/role facts, not whole-database byte immutability."""
        self.control.checkpoint = "database-observe"
        if self.value["scenario"] == "positive":
            data = self.sql(
                "final-ledger",
                (
                    # Only fixed SQL literals; no interpolation.
                    "SELECT version,success,encode(checksum,'hex') FROM "  # noqa: S608
                    + "public._sqlx_migrations ORDER BY version"
                ),
            )
            expected = {
                str(int(Path(name).name.split("_", 1)[0])): hashlib.sha384(
                    self.sources[name]
                ).hexdigest()
                for name in MIGRATIONS
            }
            rows = {
                line.split(b"|")[0].decode(): line.split(b"|")[1:] for line in data.splitlines()
            }
            require(
                len(rows) == len(data.splitlines())
                and set(rows) == set(expected)
                and all(rows[key] == [b"t", value.encode()] for key, value in expected.items())
            )
            counts = self.sql(
                "owner-lobby-counts",
                (
                    # Only fixed SQL literals; no interpolation.
                    "SELECT (SELECT count(*) FROM users),(SELECT count(*) "  # noqa: S608
                    + "FROM rooms WHERE id='lobby')"
                ),
            )
            require(counts.strip() == b"1|1")
            privileges = self.sql(
                "runtime-privileges",
                (
                    # Only fixed SQL literals; no interpolation.
                    "SELECT NOT rolsuper AND NOT rolcreatedb AND NOT "  # noqa: S608
                    + "rolcreaterole AND NOT rolreplication AND NOT "
                    + "rolbypassrls AND NOT "
                    + "has_database_privilege('simplestchat_app','simplestchat'"
                    + ",'CREATE') AND NOT "
                    + "has_schema_privilege('simplestchat_app','public','CREATE"
                    + "') AND NOT "
                    + "has_table_privilege('simplestchat_app','public._sqlx_mig"
                    + "rations','SELECT,INSERT,UPDATE,DELETE') FROM pg_roles "
                    + "WHERE rolname='simplestchat_app'"
                ),
            )
            require(privileges.strip() == b"t")
            self.observed["ownerAndLobbyObserved"] = True
            require(attempt is not None)
            receipt = self.retain(required(attempt) / "backup.sha256").data().strip()
            match = re.fullmatch(
                rb"([a-f0-9]{64})  "
                + rb"(/srv/simplestchat-public/backups/initial\.[A-Za-z0-9]{8}\.dump)",
                receipt,
            )
            require(match is not None)
            if match is not None:
                backup = self.retain(Path(match[2].decode()), 2**20)
                require(
                    backup.sha256 == match[1].decode() and backup.data(2**20).startswith(b"PGDMP")
                )
            self.observed["backupHeaderAndDigestMatched"] = True
        else:
            require(
                self.sql(
                    "unchanged-empty-schema",
                    (
                        "SELECT to_regclass('public._sqlx_migrations') IS "
                        + "NULL AND to_regclass('public.users') IS NULL AND "
                        + "to_regclass('public.rooms') IS NULL"
                    ),
                ).strip()
                == b"t"
            )
            require(
                self.sql(
                    "unchanged-source-roles",
                    (
                        # Only fixed SQL literals; no interpolation.
                        "SELECT rolname,NOT rolsuper AND NOT rolcreatedb AND "  # noqa: S608
                        + "NOT rolcreaterole AND NOT rolreplication AND NOT "
                        + "rolbypassrls FROM pg_roles WHERE rolname IN "
                        + "('simplestchat_app','simplestchat_migrate') ORDER "
                        + "BY rolname"
                    ),
                )
                == self.baseline_counts
            )
        self.observed["expectedDatabaseStateObserved"] = True

    def tls_observe(self) -> None:
        """Reuse the same seven natural originals with a fresh actual launcher-created proxy."""
        self.control.checkpoint = "tls-observe"
        for original in self.materials:
            original.check(cast("caddy.Control", cast("object", self.control)))
        self.observed["naturalMaterialsUnchanged"] = True
        if self.value["scenario"] == "positive":
            actual = self.tls(
                required(self.proxy), frozenset({required(self.app), required(self.proxy)})
            )
            caddy.bind_live_chain(actual, self.caddy_facts)
            self.observed["freshLauncherTlsObserved"] = True

    def application_observe(self) -> None:
        """Keep actual app readiness separate from the TLS-only metrics404 certificate probe."""
        self.control.checkpoint = "application-observe"
        if self.value["scenario"] == "positive":
            data = self.call(
                "actual-backend-ready",
                [
                    "curl",
                    "--disable",
                    "--noproxy",
                    "*",
                    "--proto",
                    "=http",
                    "--fail",
                    "--silent",
                    "--show-error",
                    "--max-time",
                    "2",
                    "http://127.0.0.1:3000/ready",
                ],
                5,
            )
            require(record(data).get("status") == "ready")
            self.observed["applicationReadyObserved"] = True

    def source_observe(self) -> None:
        """Keep README mutation explicit and all execution/configuration sources fixed."""
        self.control.checkpoint = "source-observe"
        self.originals_check()
        status = self.call(
            "final-source-status",
            ["git", "-C", str(self.source_root), "status", "--porcelain", "--untracked-files=all"],
            10,
        )
        require(status == (b" M README.md\n" if self.value["scenario"] == "dirty-source" else b""))
        self.observed["sourceTreeClean"] = not status
        self.observed["sourceRolesUnchanged"] = True
        self.control.check()
        lock = os.open(WORK / "workload.lock", os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC)
        first: BaseException | None = None
        try:
            self.control.check()
            info = os.fstat(lock)
            require(
                stat.S_ISREG(info.st_mode)
                and info.st_uid == 0
                and stat.S_IMODE(info.st_mode) == PRIVATE_MODE
                and info.st_nlink == 1
            )
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            self.control.check()
            require(ops.stamp((WORK / "workload.lock").lstat()) == ops.stamp(info))
            fcntl.flock(lock, fcntl.LOCK_UN)
            self.observed["workloadLockReleased"] = True
        except BaseException as error:  # noqa: BLE001 -- Settle all admitted peers.
            first = error
        first = ops.settle((lambda: os.close(lock), self.control.check), first)
        if first is not None:
            raise first

    def observe(self) -> None:
        """Bound fixed observation/known cleanup debt110 plus10 cooperative FS work inside120."""
        with self.control.observation(OBSERVATION):
            attempt = self.attempt()
            with self.control.observation(OBSERVATION_CAPS["daemon"]):
                self.daemon_observe(attempt)
            with self.control.observation(OBSERVATION_CAPS["database"]):
                self.database_observe(attempt)
            with self.control.observation(OBSERVATION_CAPS["tls"]):
                self.tls_observe()
            with self.control.observation(OBSERVATION_CAPS["application"]):
                self.application_observe()
            with self.control.observation(OBSERVATION_CAPS["source"]):
                self.source_observe()
            with self.control.observation(OBSERVATION_CAPS["collector"]):
                required(self.collector).finish()
                self.observed["collectorFinalized"] = True
            self.retire_fixture_actors()
            self.retire_fixture_network()
            self.retained_guard()

    def retire_fixture_actors(self) -> None:
        """Retire only known CIDs and register their raw-zero removal before later errors."""
        commands = required(self.commands)
        known = sorted(self.fixture_actors | set(commands.owned))
        if known:

            def removed(_data: bytes) -> None:
                self.fixture_actors.difference_update(known)
                commands.owned[:] = [value for value in commands.owned if value not in known]

            with self.control.observation(OBSERVATION_CAPS["actors"]):
                self.check_cleanup_socket()
                _ = commands.call(
                    "known-fixture-retire",
                    [
                        "docker",
                        "--host",
                        "unix:///var/run/docker.sock",
                        "rm",
                        "--force",
                        "--volumes",
                        *known,
                    ],
                    15,
                    adopt=removed,
                )
        self.observed["knownFixtureActorsRetired"] = not self.fixture_actors and not commands.owned

    def retire_fixture_network(self) -> None:
        """Retire only the exact owned network after observing its empty endpoint inventory."""
        commands = required(self.commands)
        if self.network is not None:
            identifier = self.network

            def removed(_data: bytes) -> None:
                self.network = None

            with self.control.observation(OBSERVATION_CAPS["network"]):
                self.check_cleanup_socket()
                row = caddy.one_inspection(
                    commands.call(
                        "known-network-check",
                        [
                            "docker",
                            "--host",
                            "unix:///var/run/docker.sock",
                            "network",
                            "inspect",
                            identifier,
                        ],
                        5,
                    )
                )
                require(row["Id"] == identifier and not dictionary(row["Containers"]))
                _ = commands.call(
                    "known-network-retire",
                    [
                        "docker",
                        "--host",
                        "unix:///var/run/docker.sock",
                        "network",
                        "rm",
                        identifier,
                    ],
                    max(0.001, self.control.remaining(15)),
                    adopt=removed,
                )
        self.observed["knownFixtureNetworkRetired"] = self.network is None

    def retained_guard(self) -> None:
        """Keep both existing read-only guard queries inside one original ten-second role."""
        commands = required(self.commands)
        with self.control.observation(OBSERVATION_CAPS["guard"]):
            caddy.check_guard_retained(commands, "inet", self.table)
            caddy.check_guard_retained(commands, "netdev", self.table)
            self.observed["externalGuardRetained"] = True

    def original_close_actions(self) -> list[Callable[[], None]]:
        """Enumerate all original checks/closes without skipping a peer after refusal."""
        commands = self.commands
        actions: list[Callable[[], None]] = []
        if self.watch is not None:

            def wire_finished() -> None:
                required(self.watch).finish()
                self.observed["noObservedExternalPackets"] = True

            actions.append(wire_finished)
        if self.protected is not None:
            actions.extend((self.protected.check, self.protected.close))
        for original in self.originals:
            actions.extend((original.check, original.close))
        for original in self.materials:
            actions.extend(
                (
                    lambda original=original: original.check(
                        cast("caddy.Control", cast("object", self.control))
                    ),
                    original.close,
                )
            )
        if commands is not None:
            actions.extend(
                lambda owner=owner: owner.finish(
                    cast("caddy.Control", cast("object", self.control))
                )
                for owner in commands.tool_owners.values()
            )
        return actions

    def body_cleanup_actions(self) -> list[Callable[[], None]]:
        """Enumerate collector, known daemon actors and guard observers before local settling."""
        commands = self.commands
        actions: list[Callable[[], None]] = []
        if self.collector is not None:

            def events_finished() -> None:
                required(self.collector).finish()
                self.observed["collectorFinalized"] = True

            if not required(self.collector).closed:
                actions.append(events_finished)
        if commands is not None:
            actions.extend((self.retire_fixture_actors, self.retire_fixture_network))
        if commands is not None and self.guarded and not self.observed["externalGuardRetained"]:
            actions.append(self.retained_guard)
        return actions

    def finish(self) -> None:
        """Retire only known fixture IDs under B, then close every original under original N."""
        if self.finished:
            return
        self.finished = True
        first: BaseException | None = None
        self.control.checkpoint = "cleanup"
        self.control.recovering = True
        commands = self.commands
        if self.collector is not None:
            first = self.collector.failure
        actions = self.body_cleanup_actions()
        # Retain the guard until owned-VM teardown; do not claim successful nftables deletion.
        first = ops.settle(actions, first)
        try:
            self.control.check()
        except BaseException as error:  # noqa: BLE001 -- Settle all admitted peers.
            first = first or error
        self.control.settling = True
        close_actions = self.original_close_actions()
        first = ops.settle(close_actions, first)
        self.observed["daemonEffectsUncertain"] = self.unknown_effect or (
            commands.create_uncertain if commands is not None else False
        )
        if first is not None:
            raise first

    def check_cleanup_socket(self) -> None:
        """Check actual tool/socket custody for known-ID cleanup independently of failed bulk."""
        require(self.socket is not None)
        path, before = required(self.socket)
        require(
            Path("/var/run/docker.sock").resolve(strict=True) == path
            and ops.stamp(path.lstat())[:5] == ops.stamp(before)[:5]
        )
        required(self.commands).tool_owners["docker"].check(
            cast("caddy.Control", cast("object", self.control))
        )


@dataclass(frozen=True)
class CaddyTarget:
    """A fixed actual proxy endpoint on the fixture's completely joined shared bridge."""

    fixture: Fixture
    container: str
    peers: frozenset[str]
    address: str
    prefix: int
    endpoint: str
    subnet: str
    gateway: str

    def check(self, _commands: caddy.Commands) -> None:
        """Reobserve exact IDs, IPAM and all known peers before accepting TLS facts."""
        require(self.fixture.endpoint(self.container, self.peers) == self)


CHECKPOINTS = frozenset(
    {
        "input",
        "source-import",
        "payload",
        "render",
        "database-setup",
        "caddy-natural",
        "ready",
        "native",
        "native-result",
        "daemon-observe",
        "database-observe",
        "tls-observe",
        "application-observe",
        "source-observe",
        "cleanup",
        "complete",
    }
)
FAILURES = frozenset({"io", "deadline", "cancelled", "refused", "native-status"})
OBSERVATION_FIELDS = frozenset(
    {
        "sourceCommitMatched",
        "sourceShallowBoundaryVerified",
        "sourceRolesUnchanged",
        "sourceTreeClean",
        "dirtyReadmeObserved",
        "naturalTlsPrepared",
        "naturalMaterialsUnchanged",
        "freshLauncherTlsObserved",
        "applicationReadyObserved",
        "expectedDatabaseStateObserved",
        "ownerAndLobbyObserved",
        "backupHeaderAndDigestMatched",
        "workloadLockReleased",
        "freshActorInventoryJoined",
        "collectorMarkerJoined",
        "collectorFinalized",
        "observedExpectedDaemonJoins",
        "noObservedUnexpectedMutatingEvents",
        "externalGuardRetained",
        "noObservedExternalPackets",
        "knownFixtureActorsRetired",
        "knownFixtureNetworkRetired",
        "systemToolHashesJoined",
        "daemonEffectsUncertain",
    }
)


def final_projection(value: object, expected: Mapping[str, object]) -> dict[str, object]:
    """Return only the bound closed guest metadata; actual parent/QEMU status remains separate."""
    result = exact(
        value,
        {
            "schemaVersion",
            "scope",
            "runId",
            "revision",
            "scenario",
            "passed",
            "failure",
            "checkpoint",
            "finalStatus",
            "cleanupPassed",
            "nativeRawExit",
            "nativeFrame",
            "observations",
            "sourcePins",
        },
    )
    require(type(result["schemaVersion"]) is int and result["schemaVersion"] == 1)
    require(result["scope"] == "MaintainLauncherPrerequisiteGuest")
    for name in ("runId", "revision", "scenario", "sourcePins"):
        require(result[name] == expected[name])
    require(type(result["passed"]) is bool and type(result["cleanupPassed"]) is bool)
    require(type(result["checkpoint"]) is str and result["checkpoint"] in CHECKPOINTS)
    require(
        result["failure"] is None
        or (type(result["failure"]) is str and result["failure"] in FAILURES)
    )
    require(type(result["finalStatus"]) is int and result["finalStatus"] in (0, 1, 124, 130, 143))
    raw = result["nativeRawExit"]
    require(raw is None or type(raw) is int)
    if raw is not None:
        _ = integer(raw, -127, 255)
    native = result["nativeFrame"]
    if native is not None:
        native = native_frame(encoded(native))
        result["nativeFrame"] = native
    observations = exact(result["observations"], set(OBSERVATION_FIELDS))
    require(all(type(observed) is bool for observed in observations.values()))
    if result["passed"] is True:
        require(result["failure"] is None and result["checkpoint"] == "complete")
        require(
            result["finalStatus"] == 0 and result["cleanupPassed"] is True and native is not None
        )
        scenario = cast("str", result["scenario"])
        require_native_case(required(native), integer(raw, -127, 255), scenario)
        facts = dictionary(required(native)["facts"])
        require(facts["opsSha256"] == dictionary(expected["binary"])["sha256"])
        require(
            facts["launcherSha256"]
            == dictionary(expected["sourcePins"])["ops/ansible/files/deploy-public.sh"]
        )
        positive_only = {
            "freshLauncherTlsObserved",
            "applicationReadyObserved",
            "ownerAndLobbyObserved",
            "backupHeaderAndDigestMatched",
        }
        special = positive_only | {
            "sourceTreeClean",
            "dirtyReadmeObserved",
            "daemonEffectsUncertain",
        }
        require(all(observations[name] is True for name in OBSERVATION_FIELDS - special))
        require(all(observations[name] is (scenario == "positive") for name in positive_only))
        require(observations["sourceTreeClean"] is (scenario != "dirty-source"))
        require(observations["dirtyReadmeObserved"] is (scenario == "dirty-source"))
        require(observations["daemonEffectsUncertain"] is False)
    return result


def capture_native(fixture: Fixture, report: dict[str, object]) -> None:
    """Retain actual signed status and closed passive bytes without creating execution authority."""
    native = fixture.native
    if native is None:
        return
    report["nativeRawExit"] = native.status
    if native.called and fixture.observed["observedExpectedDaemonJoins"] is not True:
        fixture.unknown_effect = True
    if native.output_complete:
        report["nativeFrame"] = native_frame(native.output)


def execute(control: Control, wire: Wire) -> dict[str, object]:
    """Run the complete fixed guest action and settle every known owner after the first refusal."""
    value = request(wire.line())
    control.configure(value)
    report: dict[str, object] = {
        "schemaVersion": 1,
        "scope": "MaintainLauncherPrerequisiteGuest",
        "runId": value["runId"],
        "revision": value["revision"],
        "scenario": value["scenario"],
        "passed": False,
        "failure": None,
        "checkpoint": "input",
        "finalStatus": 1,
        "cleanupPassed": False,
        "nativeRawExit": None,
        "nativeFrame": None,
        "observations": dict.fromkeys(OBSERVATION_FIELDS, False),
        "sourcePins": value["sourcePins"],
    }
    fixture = Fixture(value, control, wire)
    first: BaseException | None = None

    def failed(error: BaseException) -> None:
        nonlocal first
        if first is None:
            first = error
            report["failure"] = classification(error, control.status)

    try:
        fixture.prepare()
        fixture.invoke()
        fixture.observe()
        wire.no_extra()
        control.check()
    except BaseException as error:  # noqa: BLE001 -- Settle all admitted peers.
        failed(error)
    report["checkpoint"] = control.checkpoint
    try:
        capture_native(fixture, report)
    except BaseException as error:  # noqa: BLE001 -- Passive decode refusal cannot skip cleanup.
        failed(error)
    try:
        wire.no_extra()
        control.check()
    except BaseException as error:  # noqa: BLE001 -- Settle all admitted peers.
        failed(error)
    cleanup = ops.settle((fixture.finish,))
    if cleanup is not None:
        failed(cleanup)
    report["cleanupPassed"] = cleanup is None and not fixture.unknown_effect
    report["observations"] = fixture.observed
    final = ops.settle((control.check,))
    if final is not None:
        failed(final)
    report["finalStatus"] = control.status or (1 if first is not None else 0)
    report["passed"] = first is None and control.status == 0
    if report["passed"] is True:
        report["checkpoint"] = "complete"
    # This closes metadata only; actual resources were checked by their retained owners above.
    try:
        return final_projection(report, value)
    except ValueError as error:
        failed(error)
        report["passed"], report["finalStatus"] = False, control.status or 1
        return final_projection(report, value)


def restore_actions(
    wire: Wire | None, handlers: Mapping[signal.Signals, object], mask: int
) -> list[Callable[[], None]]:
    """Enumerate every owned shared-OFD/global restoration independently of previous failure."""
    actions: list[Callable[[], None]] = []
    if wire is not None:
        actions.append(wire.close)
    for number, handler in handlers.items():

        def restore(number: signal.Signals = number, handler: object = handler) -> None:
            _ = signal.signal(number, cast("int | Callable[[int, object], None] | None", handler))

        actions.append(restore)

    def restore_mask() -> None:
        _ = os.umask(mask)

    actions.append(restore_mask)
    return actions


def main() -> int:
    """Retain the original owner through global restoration, output and the final N/signal fence."""
    control = Control(time.monotonic())
    mask = os.umask(0o077)
    handlers: dict[signal.Signals, object] = {}
    wire: Wire | None = None
    report: dict[str, object] | None = None
    first: BaseException | None = None
    try:
        for number in (signal.SIGINT, signal.SIGTERM):
            handlers[number] = signal.signal(number, control.signal)
        wire = Wire(control)
        report = execute(control, wire)
    except BaseException as error:  # noqa: BLE001 -- Settle all admitted peers.
        first = error
    # A pre-execute input refusal must also observe B before widening to original N.
    if not control.settling:
        first = ops.settle((control.check,), first)
        control.settling = True
    actions = restore_actions(wire, handlers, mask)
    first = ops.settle((*actions, control.check), first)
    if report is None:
        _ = sys.stderr.write("Maintenance launcher prerequisite is unproven.\n")
        return control.status or 1
    if first is not None or control.status:
        report["passed"] = False
        report["cleanupPassed"] = False
        report["finalStatus"] = control.status or 1
        if report["failure"] is None:
            cause = first if first is not None else ControlRefusalError(control.status)
            report["failure"] = classification(cause, control.status)
    try:
        required(wire).emit(report)
        control.check()
    except BaseException:  # noqa: BLE001 -- Output refusal cannot promote success.
        return control.status or 1
    return control.status or (0 if report["passed"] is True else 1)


if __name__ == "__main__":
    raise SystemExit(main())
