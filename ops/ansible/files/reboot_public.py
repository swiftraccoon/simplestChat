"""Prepare or recover an explicitly requested reboot of one public chat host.

This helper never reboots the host. Preparation records private, durable
evidence and stops only the existing public application's three containers.
After a changed Linux boot ID, recovery starts those same containers in order.
No image build, pull, container recreation, migration or account seeding occurs.
An unsuccessful reboot request can be cancelled on the original boot only;
successful service recovery does not turn that reboot into a successful one.
"""

import argparse
import hashlib
import json
import os
import re
import signal
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from types import FrameType
from typing import NoReturn

import release_public as public
from release_artifact import sha256_file
from release_json import JsonObject, decode_json, object_value, string_value

SERVICES = ("simplestchat", "caddy", "postgres")
CONFIGURATION = {
    **dict.fromkeys(public.SELECTION, 384),
    "proxy.env": 0o600,
    "compose.base.yml": 0o644,
    "Caddyfile": 0o644,
    "pg_hba.conf": 0o644,
}
UUID = re.compile(r"[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}")
HEX = re.compile(r"[a-f0-9]{64}")
CONTAINER_FORMAT = (
    '{"id":{{json .Id}},"image":{{json .Image}},"state":{{json .State}},'
    '"configHash":{{json (index .Config.Labels "com.docker.compose.config-hash")}}}'
)


@dataclass(frozen=True, slots=True, kw_only=True)
class RebootOptions:
    """Retain the caller's explicit prepare or recovery selection."""

    action: str


class _ArgumentValues(argparse.Namespace):
    action: str = ""


def configuration() -> dict[str, str]:
    """Bind recovery to protected configuration, including mounted proxy/HBA files."""
    result: dict[str, str] = {}
    for name, mode in CONFIGURATION.items():
        path = public.CONFIG / name
        public.protected(path, modes=(mode,), limit=1024 * 1024)
        result[name] = sha256_file(path)
    return result


def container(runner: public.RunnerProtocol, service: str) -> JsonObject:
    """Inspect exactly one existing project container, whether running or stopped."""
    identifier = runner.compose("ps", "--all", "--quiet", service, timeout=10).decode().strip()
    public.require(
        HEX.fullmatch(identifier), f"Exactly one existing {service} container is required"
    )
    value = object_value(
        decode_json(
            runner.docker(
                "inspect",
                "--format",
                CONTAINER_FORMAT,
                identifier,
                timeout=10,
            )
        )
    )
    public.require(
        value["id"] == identifier
        and public.ID.fullmatch(string_value(value["image"]))
        and HEX.fullmatch(string_value(value["configHash"])),
        f"Invalid {service} container identity",
    )
    public.require(
        object_value(value["state"]).get("OOMKilled") is False, f"{service} was OOM-killed"
    )
    return value


def identity(value: JsonObject) -> JsonObject:
    """Select immutable fields used to reject replacement containers."""
    return {key: value[key] for key in ("id", "image", "configHash")}


def selection(runner: public.RunnerProtocol) -> JsonObject:
    """Verify the selected application identity before any stop or recovery."""
    value = object_value(decode_json((public.CONFIG / "images.json").read_text()))
    revision = string_value(value.get("revision"))
    image = string_value(value.get("serverImage"))
    public.require(
        re.fullmatch(r"[a-f0-9]{40}", revision) and public.ID.fullmatch(image),
        "Invalid deployed image selection",
    )
    public.require(
        public.image_identity(runner, image, revision) == image,
        "Selected application image is unavailable",
    )
    return value


def capture(runner: public.RunnerProtocol) -> JsonObject:
    """Prove a healthy, unchanged selection before taking the public service down."""
    help_text = runner.compose("start", "--help", timeout=10).decode()
    public.require(
        re.search(r"(?m)^\s+--wait\s", help_text)
        and re.search(r"(?m)^\s+--wait-timeout\s", help_text),
        "Installed Compose must support start --wait and --wait-timeout before reboot preparation",
    )
    hashes = configuration()
    selected = selection(runner)
    containers = {service: container(runner, service) for service in SERVICES}
    for service, value in containers.items():
        public.require(
            object_value(value["state"]).get("Running") is True, f"{service} must be running"
        )
        fields = runner.compose("config", "--hash", service, timeout=10).decode().split()
        public.require(
            fields == [service, value["configHash"]], "Running configuration differs from disk"
        )
    public.require(
        containers["simplestchat"]["image"] == selected["serverImage"],
        "Application image differs from selection",
    )
    for service, key in (("postgres", "postgresImage"), ("caddy", "caddyImage")):
        selector = string_value(selected.get(key))
        public.require(
            re.fullmatch(r"[^\s]+@sha256:[a-f0-9]{64}", selector),
            "Dependency images must be checksum-pinned",
        )
        image = (
            runner.docker("image", "inspect", "--format", "{{.Id}}", selector, timeout=10)
            .decode()
            .strip()
        )
        public.require(
            image == containers[service]["image"], f"{service} image differs from selection"
        )
    database_state = object_value(containers["postgres"]["state"])
    public.require(
        object_value(database_state.get("Health", {})).get("Status") == "healthy",
        "Database must be healthy",
    )
    public.require(
        not runner.compose(
            "--profile", "maintenance", "ps", "--all", "--quiet", "migrate", timeout=10
        ).strip(),
        "Inspect retained migration container first",
    )
    resolved = object_value(decode_json(runner.compose("config", "--format", "json", timeout=10)))
    services = object_value(resolved["services"])
    application = object_value(services["simplestchat"])
    environment = object_value(application["environment"])
    public.require(
        environment["RUN_MIGRATIONS"] == "false", "Runtime migrations must remain disabled"
    )
    public.require(
        all(object_value(services[service])["restart"] == "unless-stopped" for service in SERVICES),
        "Prepared reboot requires the existing unless-stopped policies",
    )
    origin = string_value(environment["WEBAUTHN_ORIGIN"])
    public.require(re.fullmatch(r"https://[a-z0-9.-]+", origin), "Unexpected public origin")
    public.ready(runner, seconds=3)
    public.ready(runner, origin=origin, seconds=3)
    return {
        "schemaVersion": 1,
        "bootId": public.boot_id(),
        "configuration": dict(hashes),
        "selection": selected,
        "containers": {service: identity(value) for service, value in containers.items()},
        "origin": origin,
        "resolvedSha256": hashlib.sha256(json.dumps(resolved, sort_keys=True).encode()).hexdigest(),
    }


def validate(runner: public.RunnerProtocol, saved: JsonObject) -> dict[str, JsonObject]:
    """Reject changed files or containers instead of rebuilding/recreating anything."""
    public.require(
        configuration() == saved["configuration"], "Configuration changed after reboot preparation"
    )
    public.require(
        selection(runner) == saved["selection"], "Image selection changed after reboot preparation"
    )
    resolved = object_value(decode_json(runner.compose("config", "--format", "json", timeout=10)))
    digest = hashlib.sha256(json.dumps(resolved, sort_keys=True).encode()).hexdigest()
    public.require(
        digest == saved["resolvedSha256"], "Resolved configuration changed after reboot preparation"
    )
    values: dict[str, JsonObject] = {}
    for service in SERVICES:
        values[service] = container(runner, service)
        public.require(
            identity(values[service]) == object_value(saved["containers"])[service],
            f"{service} container changed after preparation",
        )
    return values


def write_state(runner: public.AttemptContext, saved: JsonObject, phase: str) -> None:
    """Retain original preparation ownership throughout reboot recovery."""
    public.atomic(
        public.ROOT / "release-state.json",
        {
            "schemaVersion": 1,
            "action": "reboot",
            "attempt": str(runner.attempt),
            "finalized": False,
            "phase": phase,
            "bootId": saved["bootId"],
            "identitySha256": sha256_file(runner.attempt / "identity.json"),
        },
    )


def start(runner: public.RunnerProtocol, saved: JsonObject) -> None:
    """Start existing containers in dependency order with bounded readiness gates."""
    _ = validate(runner, saved)
    _ = runner.compose("start", "--wait", "--wait-timeout", "180", "postgres", timeout=195)
    database = container(runner, "postgres")
    state = object_value(database["state"])
    public.require(
        state.get("Running") is True
        and object_value(state.get("Health", {})).get("Status") == "healthy",
        "Database did not become healthy",
    )
    _ = runner.compose("start", "simplestchat", timeout=30)
    public.ready(runner, seconds=45)
    _ = runner.compose("start", "caddy", timeout=30)
    public.ready(runner, origin=string_value(saved["origin"]), seconds=45)
    values = validate(runner, saved)
    public.require(
        all(object_value(value["state"]).get("Running") is True for value in values.values()),
        "A recovered service is not running",
    )


def prepare(runner: public.RunnerProtocol, report: JsonObject) -> None:
    """Record exact identities, stop existing services, and recover a failed stop."""
    saved = capture(runner)
    public.atomic(runner.attempt / "identity.json", saved)
    report.update(bootId=saved["bootId"], revision=object_value(saved["selection"])["revision"])
    write_state(runner, saved, "stop_for_reboot")
    try:
        report.update(phase="stop_for_reboot", interruptionStartedAt=public.timestamp())
        for service in SERVICES:
            grace = "60" if service == "postgres" else "30"
            _ = runner.compose("stop", "--timeout", grace, service, timeout=int(grace) + 15)
            stopped = container(runner, service)
            state = object_value(stopped["state"])
            public.require(
                identity(stopped) == object_value(saved["containers"])[service]
                and state.get("Running") is False
                and state.get("ExitCode") == 0,
                f"{service} did not stop cleanly",
            )
        report["phase"] = "await_reboot"
        # Publish the evidence before admitting post-boot recovery via the journal.
        public.atomic(runner.attempt / "outcome.json", report)
        write_state(runner, saved, "await_reboot")
    except BaseException:
        report["recoveryAttempted"] = True
        try:
            public.require(public.boot_id() == saved["bootId"], "Boot changed during preparation")
            start(runner, saved)
            report.update(
                recoveryPassed=True,
                phase="prepare_failed_recovered",
                interruptionFinishedAt=public.timestamp(),
            )
            public.journal(runner, finalized=True, phase="reboot_prepare_failed_recovered")
        except BaseException:  # noqa: BLE001 - recovery must preserve the original preparation failure.
            report["recoveryPassed"] = False
        raise


def pending(action: str) -> tuple[Path, JsonObject, JsonObject]:
    """Load only the exact protected attempt selected by the persistent journal."""
    path = public.ROOT / "release-state.json"
    public.protected(path, limit=16384)
    record = object_value(decode_json(path.read_text()))
    public.require(
        type(record.get("schemaVersion")) is int
        and record["schemaVersion"] == 1
        and record.get("finalized") is False
        and record.get("action") == "reboot"
        and record.get("phase") == "await_reboot",
        "No unfinished prepared reboot is available",
    )
    public.require(
        UUID.fullmatch(string_value(record.get("bootId"))), "Invalid prepared boot identity"
    )
    public.require(
        (public.boot_id() != record["bootId"])
        if action == "resume"
        else (public.boot_id() == record["bootId"]),
        "Resume requires a changed boot; cancellation requires the original boot",
    )
    attempt = Path(string_value(record.get("attempt")))
    public.require(
        attempt.parent == public.ROOT / "results"
        and re.fullmatch(r"reboot\.[a-z0-9_]+", attempt.name)
        and str(attempt) == record["attempt"],
        "Unexpected prepared attempt path",
    )
    public.protected(attempt, directory=True, modes=(0o700,))
    snapshot = attempt / "identity.json"
    public.protected(snapshot, limit=65536)
    public.require(
        HEX.fullmatch(string_value(record.get("identitySha256")))
        and sha256_file(snapshot) == record["identitySha256"],
        "Prepared identity evidence changed",
    )
    saved = object_value(decode_json(snapshot.read_text()))
    public.require(
        type(saved.get("schemaVersion")) is int
        and saved["schemaVersion"] == 1
        and saved.get("bootId") == record["bootId"],
        "Prepared identity does not match the journal",
    )
    report_path = attempt / "outcome.json"
    public.protected(report_path, limit=65536)
    report = object_value(decode_json(report_path.read_text()))
    public.require(
        report.get("action") == "reboot"
        and report.get("passed") is False
        and report.get("phase") == "await_reboot"
        and report.get("bootId") == saved["bootId"],
        "Prepared outcome does not match the journal",
    )
    return attempt, saved, report


def recover(
    runner: public.RunnerProtocol, saved: JsonObject, report: JsonObject, action: str, attempt: Path
) -> None:
    """Recover only the prepared containers and leave cancellation explicitly failed."""
    phase = "resume_after_reboot" if action == "resume" else "cancel_before_reboot"
    journal_runner = public.JournalContext(attempt=attempt)
    write_state(journal_runner, saved, phase)
    report.update(
        phase=phase, recoveryStartedAt=public.timestamp(), recoveryBootId=public.boot_id()
    )
    start(runner, saved)
    report.update(recoveryPassed=True, interruptionFinishedAt=public.timestamp())
    if action == "resume":
        report.update(passed=True, phase="complete")
        public.journal(journal_runner, finalized=True, phase="reboot_complete")
    else:
        report.update(
            passed=False,
            phase="cancelled",
            failure="Requested reboot did not complete; original-boot services recovered",
        )
        public.journal(journal_runner, finalized=True, phase="reboot_cancelled")


def main() -> None:
    """Run one explicitly selected reboot preparation or recovery transaction."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("action", choices=("prepare", "resume", "cancel"))
    raw = parser.parse_args(namespace=_ArgumentValues())
    arguments = RebootOptions(action=raw.action)
    public.require(os.geteuid() == 0, "Run as root on the prepared public host")
    _ = os.umask(0o077)

    def interrupted(_signum: int, _frame: FrameType | None) -> NoReturn:
        message = "Reboot operation interrupted"
        raise public.ReleaseError(message)

    for signum in (signal.SIGTERM, signal.SIGINT):
        _ = signal.signal(signum, interrupted)
    with public.workload_lock(
        after_reboot=arguments.action == "resume", cancel_reboot=arguments.action == "cancel"
    ):
        public.protected(public.ROOT / "results", directory=True, modes=(0o700,))
        if arguments.action == "prepare":
            attempt = Path(tempfile.mkdtemp(prefix="reboot.", dir=public.ROOT / "results"))
            report: JsonObject = {
                "action": "reboot",
                "startedAt": public.timestamp(),
                "passed": False,
                "phase": "preflight",
            }
            runner = public.Runner(attempt)
            saved = None
        else:
            attempt, saved, report = pending(arguments.action)
            # A distinct evidence directory prevents overwriting preparation output.
            evidence = attempt / arguments.action
            evidence.mkdir(mode=0o700)
            runner = public.Runner(evidence)
        try:
            if arguments.action == "prepare":
                prepare(runner, report)
            elif saved is not None:
                recover(runner, saved, report, arguments.action, attempt)
        except BaseException as error:
            report.update(
                passed=False,
                failure=str(error)
                if isinstance(error, public.ReleaseError)
                else type(error).__name__,
            )
            raise
        finally:
            report["updatedAt"] = public.timestamp()
            public.atomic(attempt / "outcome.json", report)
            _ = sys.stdout.write(
                json.dumps(
                    {
                        "action": arguments.action,
                        "phase": report["phase"],
                        "passed": report["passed"],
                        "evidence": str(attempt),
                    }
                )
                + "\n"
            )


def cli() -> int:
    """Preserve sanitized standalone failure output and exit status."""
    try:
        main()
    except Exception as error:  # noqa: BLE001 - the standalone CLI must redact every exception message.
        failure = type(error).__name__
        _ = sys.stderr.write(
            f"Reboot operation failed ({failure}); inspect retained private evidence.\n"
        )
        return 1
    return 0
