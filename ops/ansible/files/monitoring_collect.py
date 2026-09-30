"""Sample fixed operational health signals without reading users or logging secrets."""

import datetime
import json
import os
import re
import socket
import ssl
import subprocess
import sys
import time
from http import HTTPStatus
from pathlib import Path
from typing import TYPE_CHECKING, cast
from urllib.request import ProxyHandler, build_opener

from monitoring_alerts import STATE, record
from release_json import JsonObject, decode_json, object_value, string_value
from release_public import ReleaseError, atomic, protected, require, validated_backup

if TYPE_CHECKING:
    from http.client import HTTPResponse

MAX_OUTPUT = 65536
MAX_BACKUPS = 10000
# Providers sell transfer in decimal terabytes.
TERABYTE = 1_000_000_000_000
NETWORK = Path("/sys/class/net")
MAX_RESET_DAY = 28
ROOT = Path("/srv/simplestchat-public")
SERVICES = {
    "app": "simplestchat-public-simplestchat-1",
    "postgres": "simplestchat-public-postgres-1",
    "proxy": "simplestchat-public-caddy-1",
    "turn": "simplestchat-turn-turn-1",
}
PG_QUERY = """SET statement_timeout='2s';
SELECT json_build_object('connections',numbackends,'deadlocks',deadlocks,'rollbacks',xact_rollback,
 'commits',xact_commit,'connection_limit',current_setting('max_connections')::int,
 'blocked', (SELECT count(*) FROM pg_stat_activity WHERE cardinality(pg_blocking_pids(pid)) > 0))
FROM pg_stat_database WHERE datname=current_database();"""


def command(arguments: list[str], *, seconds: float = 3) -> str:
    """Run only fixed read-only operations with bounded subprocess lifetimes."""
    result = subprocess.run(  # noqa: S603 - internal callers use fixed commands and owned paths.
        arguments,
        check=True,
        capture_output=True,
        text=True,
        timeout=seconds,
        env={"PATH": "/usr/bin:/bin", "HOME": "/root"},
    )
    require(len(result.stdout) <= MAX_OUTPUT, "Oversized monitoring command response")
    return result.stdout.strip()


def docker(*arguments: str) -> str:
    """Address the local Unix daemon, ignoring inherited Docker selectors."""
    return command(["/usr/bin/docker", "--host", "unix:///var/run/docker.sock", *arguments])


def gauge(lines: list[str], name: str, value: float, *, service: str | None = None) -> None:
    """Render fixed metric names and internally selected service labels."""
    label = f'{{service="{service}"}}' if service else ""
    lines.append(f"simplestchat_ops_{name}{label} {value}")


def containers(lines: list[str]) -> None:
    """Observe each managed container without inspecting its environment or log payloads."""
    for service, name in SERVICES.items():
        try:
            item = object_value(
                decode_json(
                    docker(
                        "inspect",
                        "--format",
                        '{"running":{{.State.Running}},"restarts":{{.RestartCount}},"oom":{{.State.OOMKilled}}}',
                        name,
                    )
                )
            )
            gauge(lines, "container_running", float(item["running"] is True), service=service)
            gauge(lines, "container_restarts", float(str(item["restarts"])), service=service)
            gauge(lines, "container_oom", float(item["oom"] is True), service=service)
            stats = object_value(
                decode_json(docker("stats", "--no-stream", "--format", "{{json .}}", name))
            )
            for source, metric in (
                ("CPUPerc", "container_cpu_percent"),
                ("MemPerc", "container_memory_percent"),
            ):
                raw = string_value(stats[source]).removesuffix("%")
                require(re.fullmatch(r"\d+(?:\.\d+)?", raw), "Invalid container sample")
                gauge(lines, metric, float(raw), service=service)
        except (OSError, ValueError, KeyError, ReleaseError, subprocess.SubprocessError):
            gauge(lines, "container_collection_ok", 0, service=service)
        else:
            gauge(lines, "container_collection_ok", 1, service=service)


def database(lines: list[str]) -> None:
    """Collect aggregate database pressure with a read-only statement and no identities."""
    try:
        output = docker(
            "exec",
            "--user",
            "postgres",
            SERVICES["postgres"],
            "psql",
            "-X",
            "-q",
            "-t",
            "-A",
            "-h",
            "/run/simplestchat-postgres",
            "-U",
            "postgres",
            "-d",
            "simplestchat",
            "-c",
            PG_QUERY,
        )
        sample = object_value(decode_json(output))
        for key in (
            "connections",
            "deadlocks",
            "rollbacks",
            "commits",
            "connection_limit",
            "blocked",
        ):
            value = sample[key]
            require(type(value) is int and value >= 0, "Invalid database sample")
            gauge(lines, f"postgres_{key}", float(str(value)))
    except (OSError, ValueError, KeyError, ReleaseError, subprocess.SubprocessError):
        gauge(lines, "postgres_collection_ok", 0)
    else:
        gauge(lines, "postgres_collection_ok", 1)


def readiness_and_tls(lines: list[str], domain: str) -> None:
    """Check public proxy readiness and the actual relay's presented certificate."""
    try:
        with cast(
            "HTTPResponse",
            build_opener(ProxyHandler({})).open(f"https://{domain}/ready", timeout=3),
        ) as response:
            ready = response.status == HTTPStatus.OK
    except OSError:
        ready = False
    gauge(lines, "public_ready", float(ready))
    try:
        with (
            socket.create_connection((domain, 5349), timeout=3) as connection,
            ssl.create_default_context().wrap_socket(connection, server_hostname=domain) as tls,
        ):
            expiry = (tls.getpeercert() or {}).get("notAfter")
            require(isinstance(expiry, str), "Missing certificate expiry")
            if isinstance(expiry, str):
                gauge(lines, "turn_certificate_expiry_seconds", ssl.cert_time_to_seconds(expiry))
    except (OSError, ValueError, ReleaseError):
        gauge(lines, "turn_tls_ok", 0)
    else:
        gauge(lines, "turn_tls_ok", 1)


def evidence(lines: list[str]) -> None:
    """Report backup/restore evidence truthfully without counting a backup as a restore."""
    # A dump file exists before pg_dump starts and survives its failure; only the
    # receipt written after durable publication counts. Verify current bytes,
    # schema and private ownership before using its recorded completion time.
    receipts = list((ROOT / "results").glob("release.*/database-before.receipt.json")) + list(
        (ROOT / "backups" / "nightly").glob("*.receipt.json")
    )
    require(len(receipts) <= MAX_BACKUPS, "Too many release backup entries")
    protected(STATE, directory=True, modes=(0o750,))
    cached: JsonObject = {}
    cache_path = STATE / "backup-integrity-cache.json"
    if cache_path.exists() or cache_path.is_symlink():
        protected(cache_path, limit=2 * 1024 * 1024)
        cache_record = object_value(decode_json(cache_path.read_bytes()))
        require(
            type(cache_record.get("schemaVersion")) is int and cache_record["schemaVersion"] == 1,
            "Invalid backup digest cache",
        )
        cached = object_value(cache_record.get("entries"))
    refreshed: JsonObject = {}
    latest = 0.0
    for receipt in receipts:
        try:
            key = str(receipt)
            entry = object_value(cached[key]) if key in cached else {}
            _dump, _record, completed = validated_backup(receipt, digest_cache=entry)
            refreshed[key] = entry
            latest = max(latest, completed)
        except (OSError, ValueError, KeyError, ReleaseError):
            continue
    atomic(cache_path, {"schemaVersion": 1, "entries": refreshed})
    gauge(lines, "backup_last_success_seconds", latest)
    # Restore exercises are separate operator-owned evidence, never inferred from pg_restore --list.
    restore = STATE / "restore-verified.timestamp"
    gauge(lines, "restore_last_success_seconds", restore.stat().st_mtime if restore.exists() else 0)
    size = command(
        ["/usr/bin/du", "-sx", "--block-size=1", str(ROOT / "releases")], seconds=5
    ).split()[0]
    gauge(lines, "release_storage_bytes", float(size))


def period_start(today: datetime.date, reset_day: int) -> datetime.date:
    """Return the first day of the billing period `today` falls in."""
    if today.day >= reset_day:
        return today.replace(day=reset_day)
    previous_month = today.replace(day=1) - datetime.timedelta(days=1)
    return previous_month.replace(day=reset_day)


def transfer_settings(configuration: JsonObject) -> tuple[str, float, int]:
    """Read the interface, allowance and reset day the collector was configured with."""
    interface = configuration.get("transfer_interface", "eth0")
    if not isinstance(interface, str) or re.fullmatch(r"[A-Za-z0-9._-]{1,15}", interface) is None:
        message = "Invalid transfer interface"
        raise ReleaseError(message)
    allowance = configuration.get("transfer_allowance_tb", 0)
    if isinstance(allowance, bool) or not isinstance(allowance, (int, float)) or allowance < 0:
        message = "Invalid transfer allowance"
        raise ReleaseError(message)
    reset_day = configuration.get("transfer_reset_day", 1)
    if (
        isinstance(reset_day, bool)
        or not isinstance(reset_day, int)
        or not 1 <= reset_day <= MAX_RESET_DAY
    ):
        message = "Invalid transfer reset day"
        raise ReleaseError(message)
    return interface, float(allowance), reset_day


def transfer(
    lines: list[str],
    configuration: JsonObject,
    *,
    network: Path = NETWORK,
    state: Path = STATE,
    today: datetime.date | None = None,
) -> None:
    """Total the interface's traffic per billing period beside the provider's allowance.

    The kernel counters restart with the interface, so the period's totals live
    in the state directory and grow by each sample's delta; a counter below its
    last reading restarted and contributes what it holds. The first sample of a
    period counts nothing: the counter's history before it is unknown.
    """
    interface, allowance_tb, reset_day = transfer_settings(configuration)
    counters = {
        direction: int(
            (network / interface / "statistics" / f"{direction}_bytes").read_text().strip()
        )
        for direction in ("tx", "rx")
    }
    period = period_start(today or datetime.datetime.now(tz=datetime.UTC).date(), reset_day)
    path = state / "transfer.json"
    previous = object_value(decode_json(path.read_bytes())) if path.exists() else {}
    same_period = previous.get("period") == period.isoformat()
    totals: dict[str, int] = {}
    for direction, counter in counters.items():
        total = previous.get(f"{direction}_bytes") if same_period else 0
        last = previous.get(f"last_{direction}") if same_period else None
        running = total if isinstance(total, int) and not isinstance(total, bool) else 0
        if isinstance(last, int) and not isinstance(last, bool):
            running += counter - last if counter >= last else counter
        totals[direction] = running
    record: JsonObject = {
        "period": period.isoformat(),
        "tx_bytes": totals["tx"],
        "rx_bytes": totals["rx"],
        "last_tx": counters["tx"],
        "last_rx": counters["rx"],
    }
    atomic(path, record)
    gauge(lines, "transfer_period_tx_bytes", totals["tx"])
    gauge(lines, "transfer_period_rx_bytes", totals["rx"])
    gauge(lines, "transfer_allowance_bytes", allowance_tb * TERABYTE)
    gauge(
        lines,
        "transfer_period_start_timestamp_seconds",
        datetime.datetime(period.year, period.month, period.day, tzinfo=datetime.UTC).timestamp(),
    )


def main() -> None:
    """Publish one atomic textfile; every missing subsystem has an explicit failure gauge."""
    require(os.geteuid() == 0, "Monitoring collection requires the prepared host")
    started = time.monotonic()
    lines: list[str] = []
    configuration = object_value(
        decode_json(Path("/etc/simplestchat-monitoring/settings.json").read_bytes())
    )
    domain = string_value(configuration["domain"])
    require(
        re.fullmatch(r"(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,63}", domain),
        "Invalid monitored domain",
    )
    selected = object_value(decode_json(Path("/etc/simplestchat-public/images.json").read_bytes()))
    revision = string_value(selected["revision"])
    require(re.fullmatch(r"[a-f0-9]{40}", revision), "Invalid monitored revision")
    containers(lines)
    database(lines)
    readiness_and_tls(lines, domain)
    try:
        evidence(lines)
    except (OSError, ValueError, ReleaseError, subprocess.SubprocessError):
        gauge(lines, "evidence_collection_ok", 0)
    else:
        gauge(lines, "evidence_collection_ok", 1)
    try:
        transfer(lines, configuration)
    except (OSError, ValueError, ReleaseError):
        gauge(lines, "transfer_collection_ok", 0)
    else:
        gauge(lines, "transfer_collection_ok", 1)
    try:
        healthy, pending, dropped = record(revision)
        gauge(lines, "recorder_ok", float(healthy))
        gauge(lines, "recorder_spool_snapshots", pending)
        gauge(lines, "recorder_spool_dropped_total", dropped)
    except (OSError, ValueError, KeyError, ReleaseError):
        gauge(lines, "recorder_ok", 0)
    gauge(lines, "collector_timestamp_seconds", time.time())
    gauge(lines, "collector_duration_seconds", time.monotonic() - started)
    target = STATE / "textfile/operations.prom"
    atomic(target, "\n".join(lines) + "\n")
    target.chmod(0o644)
    _ = sys.stdout.write(json.dumps({"event": "monitoring_collection", "completed": True}) + "\n")


if __name__ == "__main__":
    main()
