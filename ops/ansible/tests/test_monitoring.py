"""Validate privacy, retention and replay boundaries without contacting a deployment."""

import ast
import datetime
import hashlib
import io
import json
import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from test_public_templates import render
from test_support import ROOT, array, obj, objects, string, yaml_value

# isort: split
import monitoring_alerts as alerts
import monitoring_collect as collect
import release_public as release
from release_json import JsonObject, decode_json


class MonitoringTests(unittest.TestCase):
    """Keep alert data finite and failures visible while PostgreSQL is unavailable."""

    def test_backup_freshness_counts_only_integrity_checked_private_receipts(self) -> None:
        """A dump file exists before pg_dump runs; only a receipt after listing counts."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state = root / "state"
            state.mkdir(mode=0o750)
            # Match the installed directory even under an inherited private umask.
            state.chmod(0o750)
            partial = root / "results" / "release.partial"
            partial.mkdir(parents=True)
            _ = (partial / "database-before.dump").write_bytes(b"x" * 100)
            good = root / "results" / "release.good"
            good.mkdir()
            _ = (good / "database-before.dump").write_bytes(b"y" * 40)
            _ = (good / "database-before.receipt.json").write_text(
                json.dumps(
                    {
                        "schemaVersion": 1,
                        "dump": "database-before.dump",
                        "bytes": 40,
                        "sha256": hashlib.sha256(b"y" * 40).hexdigest(),
                        "completedAt": release.timestamp(),
                    }
                )
            )
            grown = root / "results" / "release.grown"
            grown.mkdir()
            _ = (grown / "database-before.dump").write_bytes(b"z" * 10)
            _ = (grown / "database-before.receipt.json").write_text(
                json.dumps({"schemaVersion": 1, "dump": "database-before.dump", "bytes": 4})
            )
            (root / "releases").mkdir()
            good.chmod(0o700)
            (good / "database-before.dump").chmod(0o600)
            (good / "database-before.receipt.json").chmod(0o600)
            with (
                patch.object(release, "ROOT_UID", os.getuid()),
                patch.object(collect, "ROOT", root),
                patch.object(collect, "STATE", state),
                patch.object(collect, "command", return_value="0\t/tmp"),
            ):
                lines: list[str] = []
                collect.evidence(lines)
            fresh = [
                line
                for line in lines
                if line.startswith("simplestchat_ops_backup_last_success_seconds ")
            ]
            self.assertEqual(len(fresh), 1)
            self.assertAlmostEqual(
                float(fresh[0].split()[1]),
                (good / "database-before.receipt.json").stat().st_mtime,
                delta=1,
            )
            with (
                patch.object(release, "ROOT_UID", os.getuid()),
                patch.object(collect, "ROOT", root),
                patch.object(collect, "STATE", state),
                patch.object(collect, "command", return_value="0\t/tmp"),
            ):
                (good / "database-before.receipt.json").unlink()
                lines = []
                collect.evidence(lines)
            self.assertIn("simplestchat_ops_backup_last_success_seconds 0", "\n".join(lines))

    def test_transfer_totals_each_billing_period_and_survives_counter_restarts(self) -> None:
        """Period totals grow by each sample's delta and start over with the period."""
        settings: JsonObject = {
            "transfer_interface": "eth0",
            "transfer_allowance_tb": 5,
            "transfer_reset_day": 5,
        }
        self.assertEqual(
            collect.period_start(datetime.date(2026, 10, 3), 5), datetime.date(2026, 9, 5)
        )
        self.assertEqual(
            collect.period_start(datetime.date(2026, 10, 5), 5), datetime.date(2026, 10, 5)
        )
        with tempfile.TemporaryDirectory() as directory:
            network = Path(directory) / "net"
            statistics = network / "eth0" / "statistics"
            statistics.mkdir(parents=True)
            state = Path(directory) / "state"
            state.mkdir()

            def sample(tx: int, rx: int, day: datetime.date) -> dict[str, str]:
                _ = (statistics / "tx_bytes").write_text(f"{tx}\n")
                _ = (statistics / "rx_bytes").write_text(f"{rx}\n")
                lines: list[str] = []
                collect.transfer(lines, settings, network=network, state=state, today=day)
                return dict(line.rsplit(" ", 1) for line in lines)

            september = datetime.date(2026, 9, 29)
            first = sample(100, 50, september)
            # The first sample of a period has no history to count.
            self.assertEqual(first["simplestchat_ops_transfer_period_tx_bytes"], "0")
            self.assertEqual(first["simplestchat_ops_transfer_allowance_bytes"], "5000000000000.0")
            second = sample(400, 50, september)
            self.assertEqual(second["simplestchat_ops_transfer_period_tx_bytes"], "300")
            self.assertEqual(second["simplestchat_ops_transfer_period_rx_bytes"], "0")
            # A counter below its last reading restarted with the interface.
            third = sample(120, 10, september)
            self.assertEqual(third["simplestchat_ops_transfer_period_tx_bytes"], "420")
            self.assertEqual(third["simplestchat_ops_transfer_period_rx_bytes"], "10")
            record = decode_json((state / "transfer.json").read_bytes())
            self.assertEqual(obj(record)["period"], "2026-09-05")
            fresh = sample(200, 20, datetime.date(2026, 10, 6))
            self.assertEqual(fresh["simplestchat_ops_transfer_period_tx_bytes"], "0")
            self.assertEqual(
                obj(decode_json((state / "transfer.json").read_bytes()))["period"], "2026-10-05"
            )
            with self.assertRaisesRegex(release.ReleaseError, "reset day"):
                collect.transfer(
                    [], dict(settings, transfer_reset_day=31), network=network, state=state
                )
            with self.assertRaisesRegex(release.ReleaseError, "interface"):
                collect.transfer(
                    [], dict(settings, transfer_interface="../x"), network=network, state=state
                )

    def test_rules_and_exported_metrics_have_a_checked_contract(self) -> None:
        """No rule can silently disappear from the recorder or reference a misspelled metric."""
        config = obj(yaml_value((ROOT / "ops/ansible/files/monitoring-alerts.yml").read_text()))
        rules = objects(array(config, "groups")[0], "rules")
        source = "\n".join(path.read_text() for path in (ROOT / "src").rglob("*.rs"))
        for rule in rules:
            self.assertIn(string(rule, "alert"), alerts.RULES)
            for match in re.finditer(r"\bsimplestchat_[a-z_]+", string(rule, "expr")):
                metric = match.group(0)
                if not metric.startswith("simplestchat_ops_"):
                    self.assertTrue(
                        metric in source
                        or (
                            metric.startswith("simplestchat_quality_")
                            and f'"{metric.removeprefix("simplestchat_quality_")}"' in source
                        ),
                        metric,
                    )

    def test_scrape_limit_matches_server_cardinality_budget(self) -> None:
        """The collector must accept the worst-case finite server instrumentation budget."""
        config = obj(yaml_value(render("monitoring-prometheus.yml.j2")))
        app = next(item for item in objects(config, "scrape_configs") if item["job_name"] == "app")
        source = "\n".join(path.read_text() for path in (ROOT / "src").rglob("*.rs"))
        match = re.search(r"const SCRAPE_SAMPLE_LIMIT: usize = ([0-9_]+);", source)
        self.assertIsNotNone(match)
        if match is not None:
            self.assertEqual(app["sample_limit"], int(match.group(1).replace("_", "")))

    def test_only_fixed_rules_and_safe_resources_enter_incidents(self) -> None:
        """Free-form content, identifiers and query-bearing labels are rejected."""
        cases: list[tuple[str, JsonObject]] = [
            ("Unknown", {}),
            ("HostDiskLow", {"email": "private@example.test"}),
            ("HostDiskLow", {"instance": "host/?credential=secret"}),
        ]
        for rule, resource in cases:
            with (
                self.subTest(rule=rule, resource=resource),
                self.assertRaises(release.ReleaseError),
            ):
                _ = alerts.incident(rule, "critical", resource)
        first = alerts.incident("ContainerUnavailable", "critical", {"service": "app"})
        second = alerts.incident("ContainerUnavailable", "warning", {"service": "app"})
        self.assertEqual(first["key"], second["key"])

    def test_unknown_rules_and_unavailable_prometheus_never_resolve_existing_incidents(
        self,
    ) -> None:
        """An incomplete alert snapshot cannot look like an empty healthy one."""
        response = io.BytesIO(
            json.dumps(
                {
                    "status": "success",
                    "data": {
                        "alerts": [
                            {
                                "state": "firing",
                                "labels": {"alertname": "Unknown", "severity": "warning"},
                            },
                        ]
                    },
                }
            ).encode()
        )
        opener = Mock(open=Mock(return_value=response))
        with patch.object(alerts, "build_opener", return_value=opener):
            value = alerts.snapshot(None)
        self.assertIs(value["complete"], expr2=False)
        self.assertEqual(obj(array(value, "alerts")[0])["rule"], "PrometheusUnavailable")

    def test_spool_preserves_oldest_and_latest_and_counts_discarded_snapshots(self) -> None:
        """An outage cannot consume unbounded disk and data loss is explicitly counted."""
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(alerts, "STATE", Path(directory)),
            patch.object(alerts, "MAX_SNAPSHOTS", 3),
        ):
            for number in range(3):
                self.assertEqual(alerts.persist_spool({"number": number})[1], 0)
            self.assertEqual(alerts.persist_spool({"number": 3})[1], 1)
            retained = sorted((Path(directory) / "spool").glob("*.json"))
            self.assertEqual(
                [obj(decode_json(path.read_bytes()))["number"] for path in retained],
                [0, 2, 3],
            )

    def test_failed_database_replays_same_snapshot_including_database_failure(self) -> None:
        """Retry after interruption preserves the incident; committed snapshots are removed."""
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(alerts, "STATE", Path(directory)),
            patch.object(alerts, "ALERT_SQL", ROOT / "ops/ansible/files/monitoring-alerts.sql"),
            patch.object(
                alerts,
                "snapshot",
                return_value={
                    "observedAt": "2026-09-23T10:00:00Z",
                    "revision": None,
                    "complete": True,
                    "alerts": [],
                },
            ),
            patch.object(subprocess, "run", side_effect=subprocess.TimeoutExpired("fixture", 1)),
        ):
            healthy, pending, dropped = alerts.record(None)
            self.assertFalse(healthy)
            self.assertEqual((pending, dropped), (1, 0))
            entry = next((Path(directory) / "spool").glob("*.json"))
            value = obj(decode_json(entry.read_bytes()))
            self.assertIs(value["complete"], expr2=False)
            self.assertEqual(obj(array(value, "alerts")[0])["rule"], "DatabaseUnavailable")
            with patch.object(subprocess, "run", return_value=subprocess.CompletedProcess([], 0)):
                healthy, pending, _ = alerts.record(None)
            self.assertTrue(healthy)
            self.assertEqual(pending, 0)

    def test_backward_clock_keeps_snapshot_order_and_updates_the_exact_failed_entry(self) -> None:
        """A clock correction cannot overwrite an older incident or regress the replay cursor."""
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(alerts, "STATE", Path(directory)),
            patch.object(alerts, "ALERT_SQL", ROOT / "ops/ansible/files/monitoring-alerts.sql"),
            patch("time.time_ns", side_effect=[200, 100]),
            patch.object(
                alerts,
                "snapshot",
                return_value={
                    "observedAt": "2020-01-01T00:00:00+00:00",
                    "revision": None,
                    "complete": True,
                    "alerts": [],
                },
            ),
            patch.object(subprocess, "run", side_effect=subprocess.TimeoutExpired("fixture", 1)),
        ):
            original: JsonObject = {
                "observedAt": "2030-01-01T00:00:00+00:00",
                "revision": None,
                "complete": True,
                "alerts": [],
            }
            first, _ = alerts.persist_spool(original)
            healthy, pending, _ = alerts.record(None)
            self.assertFalse(healthy)
            self.assertEqual(pending, 2)
            self.assertEqual(obj(decode_json(first.read_bytes())), original)
            newest = max((Path(directory) / "spool").glob("*.json"))
            self.assertGreater(newest.name, first.name)
            value = obj(decode_json(newest.read_bytes()))
            self.assertGreater(string(value, "observedAt"), string(original, "observedAt"))
            self.assertEqual(obj(array(value, "alerts")[0])["rule"], "DatabaseUnavailable")

    def test_operational_python_gates_survive_optimization(self) -> None:
        """Embedded privileged checks must never depend on removable Python assertions."""
        checked = 0
        for path in [
            *(ROOT / "ops/ansible").glob("*.yml"),
            *(ROOT / "ops/ansible/templates").glob("*.j2"),
            ROOT / "ops/ansible/files/deploy-public.sh",
        ]:
            for line in path.read_text().splitlines():
                if re.match(r"\s*assert\s", line):
                    self.fail(f"Optimization-removable operational check: {path.name}")
                if "raise SystemExit(" in line:
                    statement = ast.parse(line.strip()).body[0]
                    # Each actual fail-closed branch remains active even with -OO.
                    compiled = compile(
                        ast.fix_missing_locations(ast.Module(body=[statement], type_ignores=[])),
                        path.name,
                        "exec",
                        optimize=2,
                    )
                    with self.assertRaises(SystemExit):
                        exec(compiled, {})  # noqa: S102 - only repository-owned literal failure branches, no external input.
                    checked += 1
        self.assertGreater(checked, 30)


if __name__ == "__main__":
    _ = unittest.main()
