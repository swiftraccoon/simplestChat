"""Record allowlisted Prometheus incidents with a bounded, replayable private spool."""

import hashlib
import json
import re
import subprocess
import time
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import TYPE_CHECKING, cast
from urllib.request import ProxyHandler, build_opener

from release_json import JsonObject, array_value, decode_json, object_value, string_value
from release_public import atomic, require

if TYPE_CHECKING:
    from http.client import HTTPResponse

STATE = Path("/var/lib/simplestchat-monitoring")
MAX_BYTES = 65536
MAX_ALERTS = 100
MAX_SNAPSHOTS = 1440
RULES = frozenset(
    {
        "ExternalImportUnavailable",
        "ExternalCoverageIncomplete",
        "ExternalChecksStale",
        "ExternalCheckFailed",
        "ExternalSpoolLoss",
        "TelemetrySeriesDropped",
        "TurnMetricsUnavailable",
        "TurnAllocationPressure",
        "AdmissionRefusals",
        "ApplicationUnavailable",
        "MetricsUnavailable",
        "QualitySampleStale",
        "QualityBudgetExhausted",
        "WorkerDeath",
        "HostDiskLow",
        "HostMemoryLow",
        "ContainerUnavailable",
        "ContainerRestart",
        "ContainerMemoryPressure",
        "PostgresUnavailable",
        "PostgresConnectionPressure",
        "PublicReadinessFailed",
        "TurnCertificateExpiring",
        "TurnTLSUnavailable",
        "CollectorStale",
        "CollectorFailed",
        "BackupStale",
        "RestoreEvidenceMissing",
        "RecorderUnavailable",
        "RecorderSpoolLoss",
        "PrometheusUnavailable",
        "DatabaseUnavailable",
        "ServerErrors",
        "DatabasePoolPressure",
    }
)
ALERT_SQL = Path("/usr/local/libexec/simplestchat-public/monitoring-alerts.sql")
RESOURCES = frozenset({"service", "job", "instance", "component"})


def incident(rule: str, severity: str, resource: JsonObject) -> JsonObject:
    """Use only fixed rules and bounded resource labels, without free-form annotations."""
    require(rule in RULES and severity in ("warning", "critical"), "Unknown alert rule")
    require(set(resource) <= RESOURCES, "Unknown resource label")
    for value in resource.values():
        require(
            isinstance(value, str) and re.fullmatch(r"[A-Za-z0-9_.:/-]{1,96}", value),
            "Invalid alert label",
        )
    identity = json.dumps([rule, resource], sort_keys=True, separators=(",", ":"))
    return {
        "key": hashlib.sha256(identity.encode()).hexdigest(),
        "rule": rule,
        "severity": severity,
        "resource": resource,
    }


def snapshot(revision: str | None) -> JsonObject:
    """Read one bounded local rules response; failure never resolves existing incidents."""
    result: JsonObject = {
        "observedAt": datetime.now(UTC).isoformat(timespec="microseconds"),
        "revision": revision,
        "complete": False,
        "alerts": [],
    }
    alerts = array_value(result["alerts"])
    try:
        opener = build_opener(ProxyHandler({}))
        with cast(
            "HTTPResponse", opener.open("http://127.0.0.1:9090/api/v1/alerts", timeout=3)
        ) as response:
            data = response.read(MAX_BYTES + 1)
        require(len(data) <= MAX_BYTES, "Oversized alert response")
        value = object_value(decode_json(data))
        require(value.get("status") == "success", "Alert response failed")
        entries = array_value(object_value(value["data"])["alerts"])
        require(len(entries) <= MAX_ALERTS, "Too many active alerts")
        for entry in entries:
            alert = object_value(entry)
            if alert.get("state") != "firing":
                continue
            labels = object_value(alert["labels"])
            alerts.append(
                incident(
                    string_value(labels["alertname"]),
                    string_value(labels["severity"]),
                    {key: item for key, item in labels.items() if key in RESOURCES},
                )
            )
        result["complete"] = True
    except (OSError, ValueError, KeyError, RuntimeError):
        alerts.clear()
        alerts.append(incident("PrometheusUnavailable", "critical", {}))
    return result


def persist_spool(value: JsonObject) -> tuple[Path, int]:
    """Keep at most one day of minute snapshots; report every discarded snapshot."""
    spool = STATE / "spool"
    spool.mkdir(mode=0o700, exist_ok=True)
    data = json.dumps(value, separators=(",", ":")).encode()
    require(len(data) <= MAX_BYTES, "Oversized incident snapshot")
    entries = sorted(spool.glob("*.json"))
    sequence = max(time.time_ns(), int(entries[-1].stem) + 1 if entries else 0)
    destination = spool / f"{sequence:020}.json"
    atomic(destination, data)
    entries.append(destination)
    dropped = max(0, len(entries) - MAX_SNAPSHOTS)
    # Preserve the earliest retained observation and newest evidence. Gaps are
    # visible through a durable counter; never grow without a disk bound.
    for path in entries[1 : 1 + dropped]:
        path.unlink()
    return destination, dropped


def record(revision: str | None) -> tuple[bool, int, int]:
    """Replay snapshots in timestamp order; failed SQL leaves evidence intact."""
    state_path = STATE / "recorder.json"
    state = object_value(decode_json(state_path.read_bytes())) if state_path.exists() else {}
    value = snapshot(revision)
    retained = sorted((STATE / "spool").glob("*.json"))
    # The persisted watermark precedes DB writes, so a backward wall-clock step
    # cannot make a new observation look like an already committed replay.
    watermarks = [string_value(state.get("lastObservedAt", value["observedAt"]))]
    if retained:
        watermarks.append(
            string_value(object_value(decode_json(retained[-1].read_bytes()))["observedAt"])
        )
    observed = datetime.fromisoformat(string_value(value["observedAt"]))
    previous = max(datetime.fromisoformat(item) for item in watermarks)
    value["observedAt"] = max(observed, previous + timedelta(microseconds=1)).isoformat(
        timespec="microseconds"
    )
    state["lastObservedAt"] = value["observedAt"]
    atomic(state_path, state)
    current_path, discarded = persist_spool(value)
    dropped = int(string_value(state.get("dropped", "0"))) + discarded
    spool = sorted((STATE / "spool").glob("*.json"))
    succeeded = True
    sql = ALERT_SQL.read_bytes()
    deadline = time.monotonic() + 12
    for path in spool[:120]:
        if time.monotonic() >= deadline:
            break
        data = path.read_bytes()
        require(len(data) <= MAX_BYTES, "Oversized retained incident snapshot")
        try:
            _ = subprocess.run(  # noqa: S603 - fixed local Docker/psql invocation; JSON is quoted by psql.
                [
                    "/usr/bin/docker",
                    "--host",
                    "unix:///var/run/docker.sock",
                    "exec",
                    "-i",
                    "--user",
                    "postgres",
                    "simplestchat-public-postgres-1",
                    "psql",
                    "-X",
                    "-q",
                    "-h",
                    "/run/simplestchat-postgres",
                    "-U",
                    "postgres",
                    "-d",
                    "simplestchat",
                    "--set",
                    f"payload={data.decode()}",
                ],
                input=sql,
                capture_output=True,
                check=True,
                timeout=7,
            )
        except (OSError, subprocess.SubprocessError):
            succeeded = False
            # Preserve this failure even if PostgreSQL returns before the next scrape.
            array_value(value["alerts"]).append(incident("DatabaseUnavailable", "critical", {}))
            value["complete"] = False
            previous = datetime.fromisoformat(string_value(value["observedAt"]))
            value["observedAt"] = max(
                datetime.now(UTC), previous + timedelta(microseconds=1)
            ).isoformat(timespec="microseconds")
            atomic(current_path, value)
            break
        path.unlink()
    atomic(
        state_path,
        {"dropped": str(dropped), "databaseOk": succeeded, "lastObservedAt": value["observedAt"]},
    )
    return succeeded, len(list((STATE / "spool").glob("*.json"))), dropped
