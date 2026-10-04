"""Migration identity, durable recovery barriers and actual logical restore evidence."""

import difflib
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
import uuid
from pathlib import Path
from typing import cast
from unittest.mock import Mock, call, patch
from urllib.parse import urlsplit, urlunsplit

from test_support import ROOT

# isort: split
import bounded_process
import migrate_public as migration
import migration_snapshot as snapshot
import release_public as release
from release_json import JsonObject, JsonValue, decode_json, object_value

REQUEST: JsonObject = {
    "operationId": "a" * 32,
    "sourceOrigin": "https://old.example.test",
    "destinationOrigin": "https://new.example.test",
    "targetRevision": "b" * 40,
}


class MigrationPolicyTests(unittest.TestCase):
    """Reject ambiguous inputs and unsafe lifecycle transitions before commands run."""

    def test_request_has_exact_fields_and_canonical_origins(self) -> None:
        """Credentials, paths, shell fragments and silent transport extensions are refused."""
        self.assertEqual(migration.parse_request(REQUEST).json(), REQUEST)
        same_origin = {**REQUEST, "destinationOrigin": REQUEST["sourceOrigin"]}
        self.assertEqual(migration.parse_request(same_origin).json(), same_origin)
        for field, value in (
            ("operationId", "../escape"),
            ("operationId", "A" * 32),
            ("targetRevision", "main"),
            ("sourceOrigin", "http://old.example.test"),
            ("sourceOrigin", "https://old.example.test/"),
            ("sourceOrigin", "https://user:password@old.example.test"),
            ("sourceOrigin", "https://old.example.test:443"),
            ("destinationOrigin", "https://new.example.test/path"),
            ("destinationOrigin", "https://new.example.test:443"),
            ("sourceOrigin", "https://x';select.example.test"),
        ):
            with self.subTest(field=field, value=value), self.assertRaises(release.ReleaseError):
                _ = migration.parse_request({**REQUEST, field: value})
        with self.assertRaises(release.ReleaseError):
            _ = migration.parse_request({**REQUEST, "force": True})
        with self.assertRaises(ValueError):
            _ = migration.parse_request(decode_json('{"operationId":"a","operationId":"b"}'))

    def test_snapshot_compares_every_field_except_independent_cluster_id(self) -> None:
        """Equal counts cannot hide changed data, grants, sequences or migration checksums."""
        original: JsonObject = {
            "passed": True,
            "systemIdentifier": "1",
            "tables": [{"schema": "public", "name": "users", "rows": 6, "sha256": "a" * 64}],
            "sequences": [{"lastValue": "9", "isCalled": True}],
            "schemaMetadata": {"sha256": "b" * 64},
            "migrationLedger": ["c" * 96],
        }
        target: JsonObject = {**original, "systemIdentifier": "2"}
        snapshot.equivalent(original, target)
        for key, changed in (
            ("tables", [{"schema": "public", "name": "users", "rows": 6, "sha256": "d" * 64}]),
            ("sequences", [{"lastValue": "10", "isCalled": True}]),
            ("schemaMetadata", {"sha256": "e" * 64}),
            ("migrationLedger", ["f" * 96]),
            ("systemIdentifier", "1"),
            ("passed", False),
        ):
            with self.subTest(key=key), self.assertRaises(release.ReleaseError):
                snapshot.equivalent(original, {**target, key: cast("JsonValue", changed)})

    def test_sealed_source_can_never_recover(self) -> None:
        """Recovery refusal precedes filesystem reads and service starts."""
        request = migration.parse_request(REQUEST)
        runner = Mock(spec=release.RunnerProtocol)
        for phase in ("sealing", "cutover-sealed", "retiring", "retired", "restored"):
            with self.subTest(phase=phase), self.assertRaises(release.ReleaseError):
                _ = migration.recover(runner, request, {"phase": phase})
        self.assertEqual(runner.mock_calls, [])

    def test_recovered_source_is_verified_without_starting_services_again(self) -> None:
        """Controller recovery after local recovery is safe and has no duplicate mutation."""
        request = migration.parse_request(REQUEST)
        runner = Mock(spec=release.RunnerProtocol)
        compose = cast("Mock", runner.compose)
        compose.side_effect = [b"postgres hash", b"simplestchat hash", b"caddy hash"]
        container: JsonObject = {
            "id": "1" * 64,
            "image": "sha256:" + "2" * 64,
            "configHash": "hash",
            "running": True,
        }
        prior: JsonObject = {
            "phase": "source-recovered",
            "before": {
                "revision": "c" * 40,
                "configurationSha256": {"app.env": "hash"},
                "containers": dict.fromkeys(("postgres", "simplestchat", "caddy"), container),
            },
        }
        with (
            patch.object(
                migration,
                "read_object",
                return_value={
                    "schemaVersion": 1,
                    "operation": "server_migration",
                    "request": REQUEST,
                    "phase": "source-recovered",
                },
            ),
            patch.object(
                migration,
                "configuration",
                return_value=(
                    {"revision": "c" * 40},
                    {"ALLOWED_ORIGINS": request.source_origin},
                ),
            ),
            patch.object(migration, "configuration_hashes", return_value={"app.env": "hash"}),
            patch.object(migration, "owned_container", return_value=container),
            patch.object(release, "ready") as ready,
            patch.object(migration, "record") as recorded,
        ):
            self.assertEqual(migration.recover(runner, request, prior), prior)
        self.assertEqual(ready.call_count, 2)
        recorded.assert_not_called()
        self.assertEqual(
            compose.call_args_list,
            [
                call("config", "--hash", service, timeout=15)
                for service in ("postgres", "simplestchat", "caddy")
            ],
        )
        cast("Mock", runner.run).assert_not_called()
        cast("Mock", runner.docker).assert_not_called()

    def test_retirement_stops_only_owned_services_and_keeps_history(self) -> None:
        """Monitoring retirement changes lifecycle only, without pruning or deleting data."""
        runner = Mock(spec=release.RunnerProtocol)
        with (
            patch.object(migration, "record", return_value={}),
            patch.object(migration, "units", return_value={}),
            patch.object(
                migration,
                "owned_container",
                return_value={"id": "1" * 64, "running": False, "restartPolicy": "no"},
            ) as owned,
        ):
            _ = migration.retire(
                runner, migration.parse_request(REQUEST), {"phase": "cutover-sealed"}
            )
        self.assertEqual(owned.call_count, 9)
        for service in ("prometheus", "node-exporter"):
            owned.assert_any_call(runner, service, optional=True, project="simplestchat-monitoring")
        expected = [
            operation
            for duration in ("30", "3", "30")
            for operation in (
                call("update", "--restart=no", "1" * 64, timeout=15),
                call("stop", "--time", duration, "1" * 64, timeout=45),
            )
        ]
        expected.extend(
            3
            * [
                call("stop", "--time", "10", "1" * 64, timeout=20),
                call("update", "--restart=no", "1" * 64, timeout=15),
            ]
        )
        self.assertEqual(cast("Mock", runner.docker).call_args_list, expected)
        cast("Mock", runner.compose).assert_not_called()
        cast("Mock", runner.run).assert_not_called()

    def test_verify_target_refuses_missing_or_failed_restore_before_reading_data(self) -> None:
        """Direct playbook invocation cannot promote an empty or partial target."""
        request = migration.parse_request(REQUEST)
        runner = Mock(spec=release.RunnerProtocol)
        for prior in (
            {},
            {"role": "destination", "phase": "restoring"},
            {"role": "source", "phase": "restored"},
        ):
            with self.subTest(prior=prior), self.assertRaises(release.ReleaseError):
                _ = migration.verify_target(runner, request, cast("JsonObject", prior))
        self.assertEqual(runner.mock_calls, [])

    def test_owner_update_is_cluster_bound_and_preserves_non_email_fields(self) -> None:
        """The supported update cannot rotate credentials or change ownership identifiers."""
        sql = migration.owner_sql("https://old.example.test", "https://new.example.test", "123")
        self.assertIn("pg_control_system()).system_identifier::text<>'123'", sql)
        self.assertIn("UPDATE public.users SET email='owner@new.example.test'", sql)
        self.assertIn("to_jsonb(original)-'email'", sql)
        self.assertIn("INTO STRICT original", sql)
        self.assertIn("GET DIAGNOSTICS affected=ROW_COUNT", sql)
        for malicious in ("", "123;COMMIT", "-1", "1'", "x"):
            with self.subTest(identity=malicious), self.assertRaises(release.ReleaseError):
                _ = migration.owner_sql(
                    "https://old.example.test", "https://new.example.test", malicious
                )

    def test_catalog_identifiers_cannot_change_copy_query(self) -> None:
        """Even unusual retained catalog names stay inside one quoted identifier."""
        self.assertEqual(
            snapshot.identifier('x"; DROP TABLE users; --'), '"x""; DROP TABLE users; --"'
        )
        for invalid in ("", "x" * 64, "x\x00y"):
            with self.assertRaises(release.ReleaseError):
                _ = snapshot.identifier(invalid)

    def test_freeze_failure_attempts_source_recovery_and_preserves_failure(self) -> None:
        """A failed archive or quiescence check never silently leaves a successful freeze."""
        request = migration.parse_request(REQUEST)
        runner = Mock(spec=release.RunnerProtocol)
        before: JsonObject = {
            "containers": {"postgres": {"id": "1" * 64}},
            "postgresSelector": "pin",
        }

        def record(_request: migration.Request, phase: str, details: JsonObject) -> JsonObject:
            return {**details, "phase": phase}

        with (
            patch.object(migration, "state", return_value=None),
            patch.object(migration, "inspect", return_value=before),
            patch.object(migration, "pinned_postgres"),
            patch.object(migration, "units", return_value={}),
            patch.object(
                migration,
                "record",
                side_effect=record,
            ),
            patch.object(migration, "stop_units"),
            patch.object(migration, "stopped"),
            patch.object(
                migration, "no_clients", side_effect=release.ReleaseError("active_writer")
            ),
            patch.object(migration, "recover", return_value={}) as recovery,
        ):
            with self.assertRaisesRegex(release.ReleaseError, "active_writer"):
                _ = migration.freeze(runner, request)
            recovery.assert_called_once()

    def test_unowned_unit_names_are_never_operated(self) -> None:
        """Retained evidence cannot introduce system services outside the fixed allowlist."""
        with self.assertRaises(release.ReleaseError):
            _ = migration.unit_names({"ssh.service": {"loaded": True}})

    def test_optional_unit_not_found_status_does_not_abort_migration(self) -> None:
        """Absent backup services are explicit observations and never selected for mutation."""
        name = "simplestchat-backup-upload.service"
        with patch.object(
            bounded_process,
            "run",
            return_value=(4, b"LoadState=not-found\nActiveState=inactive\nUnitFileState=\n", b""),
        ) as command:
            values = migration.units((name,))
        self.assertEqual(values, {name: {"loaded": False, "active": False, "enabled": ""}})
        self.assertEqual(migration.unit_names(values), [])
        command.assert_called_once_with(
            ["/usr/bin/systemctl", "show", name, "--property=LoadState,ActiveState,UnitFileState"],
            env=release.ENV,
            limits=bounded_process.Limits(timeout=10, stdout=4096, stderr=4096),
        )

    def test_unit_inspection_refuses_bus_errors_partial_or_ambiguous_output(self) -> None:
        """A missing optional unit must not disguise a failed or ambiguous host inspection."""
        for status, output in (
            (1, b""),
            (1, b"Failed to connect to bus\n"),
            (4, b"LoadState=not-found\n"),
            (4, b"LoadState=not-found\nActiveState=active\n"),
            (1, b"LoadState=loaded\nActiveState=active\nUnitFileState=enabled\n"),
            (0, b"LoadState=loaded\nActiveState=active\n"),
            (0, b"LoadState=loaded\nLoadState=not-found\nActiveState=inactive\n"),
        ):
            with (
                self.subTest(status=status, output=output),
                patch.object(bounded_process, "run", return_value=(status, output, b"")),
                self.assertRaises(release.ReleaseError),
            ):
                _ = migration.units(("simplestchat-backup-upload.service",))

    def test_loaded_unit_requires_success_and_retains_active_recovery_state(self) -> None:
        """Installed writers remain selected for stopping and only active ones for recovery."""
        name = "simplestchat-monitoring.timer"
        for state in ("active", "activating", "reloading", "deactivating"):
            with (
                self.subTest(state=state),
                patch.object(
                    bounded_process,
                    "run",
                    return_value=(
                        0,
                        f"LoadState=loaded\nActiveState={state}\nUnitFileState=enabled\n".encode(),
                        b"",
                    ),
                ),
            ):
                values = migration.units((name,))
            self.assertEqual(migration.unit_names(values), [name])
            self.assertEqual(
                migration.unit_names(values, active=True), [] if state == "deactivating" else [name]
            )

    def test_archive_failure_precedes_every_destination_start(self) -> None:
        """An absent or changed archive cannot initialize a destination database."""
        runner = Mock(spec=release.RunnerProtocol)
        with (
            patch.object(migration, "state", return_value=None),
            patch.object(
                migration, "verified_archive", side_effect=release.ReleaseError("changed")
            ),
            self.assertRaises(release.ReleaseError),
        ):
            _ = migration.restore(runner, migration.parse_request(REQUEST))
        self.assertEqual(runner.mock_calls, [])

    def test_changed_frozen_database_cannot_be_sealed(self) -> None:
        """The irreversible recovery barrier requires the original frozen data."""
        runner = Mock(spec=release.RunnerProtocol)
        with (
            patch.object(migration, "stopped"),
            patch.object(migration, "verified_archive", return_value=({}, {"rows": "original"})),
            patch.object(migration, "owned_container", return_value={"id": "1" * 64}),
            patch.object(migration, "no_clients"),
            patch.object(snapshot, "collect", return_value={"rows": "changed"}),
            patch.object(migration, "record") as recorded,
            self.assertRaises(release.ReleaseError),
        ):
            _ = migration.seal(runner, migration.parse_request(REQUEST), {"phase": "frozen"})
        recorded.assert_not_called()

    def test_wrong_compose_labels_refuse_container_operations(self) -> None:
        """An exact-looking container name alone never establishes project ownership."""
        runner = Mock(spec=release.RunnerProtocol)
        docker = cast("Mock", runner.docker)
        docker.side_effect = [
            b"1" * 64,
            json.dumps(
                {
                    "id": "1" * 64,
                    "name": "/simplestchat-public-postgres-1",
                    "image": "sha256:" + "2" * 64,
                    "oom": False,
                    "project": "unrelated",
                    "service": "postgres",
                }
            ).encode(),
        ]
        with self.assertRaises(release.ReleaseError):
            _ = migration.owned_container(runner, "postgres")
        self.assertEqual(docker.call_count, 2)

    def test_owner_snapshot_rewrites_only_the_managed_users_digest(self) -> None:
        """The post-update comparison still covers every other table and all permissions."""
        original: JsonObject = {
            "tables": [
                {"schema": "public", "name": "users", "rows": 6, "sha256": "old"},
                {"schema": "public", "name": "rooms", "rows": 4, "sha256": "rooms"},
            ],
            "schemaMetadata": {"sha256": "schema"},
            "sequences": [],
        }
        with patch.object(snapshot, "renamed_users", return_value={"rows": 6, "sha256": "new"}):
            expected = migration.expected_owner_snapshot(
                "1" * 64, migration.parse_request(REQUEST), original
            )
        self.assertNotEqual(expected, original)
        self.assertEqual(expected["schemaMetadata"], original["schemaMetadata"])
        self.assertEqual(
            cast("list[JsonValue]", expected["tables"])[1],
            cast("list[JsonValue]", original["tables"])[1],
        )
        self.assertEqual(
            object_value(cast("list[JsonValue]", original["tables"])[0])["sha256"], "old"
        )

    def test_same_origin_restore_keeps_exact_rows_without_owner_sql_or_projection(self) -> None:
        """Host relocation preserves the owner and still refuses any unexpected data change."""
        request = migration.parse_request({**REQUEST, "destinationOrigin": REQUEST["sourceOrigin"]})
        image = "sha256:" + "c" * 64
        source: JsonObject = {
            "passed": True,
            "systemIdentifier": "1",
            "postgresVersion": "180006",
            "tables": [{"schema": "public", "name": "users", "rows": 6, "sha256": "original"}],
        }
        restored: JsonObject = {**source, "systemIdentifier": "2"}
        archive: JsonObject = {
            "source": {"machineId": "source", "rpId": "old.example.test"},
            "systemIdentifier": "1",
            "postgresSelector": "pin",
            "postgresImage": image,
        }
        for changed in (False, True):
            runner = Mock(spec=release.RunnerProtocol)
            final = {**restored, "unexpected": True} if changed else restored

            def record(
                _request: migration.Request,
                phase: str,
                details: JsonObject,
                *,
                finalized: bool = False,
            ) -> JsonObject:
                return {**details, "phase": phase, "finalized": finalized}

            with (
                self.subTest(changed=changed),
                patch.object(migration, "state", return_value=None),
                patch.object(migration, "verified_archive", return_value=(archive, source)),
                patch.object(
                    migration,
                    "inspect",
                    return_value={
                        "machineId": "target",
                        "rpId": "old.example.test",
                        "postgresSelector": "pin",
                    },
                ),
                patch.object(migration, "record", side_effect=record),
                patch.object(
                    migration, "owned_container", return_value={"id": "1" * 64, "image": image}
                ),
                patch.object(
                    migration,
                    "database_identity",
                    return_value={
                        "emptyDatabase": True,
                        "systemIdentifier": "2",
                        "serverVersion": "180006",
                    },
                ),
                patch.object(migration, "pinned_postgres"),
                patch.object(migration, "stopped"),
                patch.object(migration, "no_clients"),
                patch.object(snapshot, "collect", side_effect=[restored, final]),
                patch.object(release, "atomic") as atomic,
                patch.object(migration, "sha256_file", return_value="d" * 64),
                patch.object(migration, "expected_owner_snapshot") as projection,
                patch.object(migration, "owner_sql") as owner_statement,
                patch.object(migration, "sql") as query,
                patch.object(migration, "retain_destination_owner") as owner_file,
            ):
                if changed:
                    with self.assertRaisesRegex(
                        release.ReleaseError, "unexpected_owner_rename_changes"
                    ):
                        _ = migration.restore(runner, request)
                else:
                    result = migration.restore(runner, request)
                    self.assertEqual(result["phase"], "restored")
                    self.assertIs(result["ownerEmailChanged"], expr2=False)
                    atomic.assert_called_with(request.directory / "database-renamed.json", restored)
                projection.assert_not_called()
                owner_statement.assert_not_called()
                query.assert_not_called()
                owner_file.assert_called_once_with(request)
            self.assertEqual(
                cast("Mock", runner.docker).call_count, 1, "only pg_restore writes data"
            )

    def test_seal_records_recovery_barrier_before_disabling_source_writers(self) -> None:
        """A failed shutdown cannot claim a completed seal or reopen recovery."""
        request = migration.parse_request(REQUEST)
        for failed in (False, True):
            events: list[str] = []

            def record(
                _request: migration.Request,
                phase: str,
                details: JsonObject,
                captured: list[str] = events,
            ) -> JsonObject:
                captured.append(phase)
                return {**details, "phase": phase}

            def stop(
                _runner: release.RunnerProtocol,
                captured: list[str] = events,
                *,
                fail: bool = failed,
            ) -> None:
                captured.append("stop-writers")
                if fail:
                    message = "shutdown_failed"
                    raise release.ReleaseError(message)

            with (
                self.subTest(failed=failed),
                patch.object(migration, "stopped"),
                patch.object(
                    migration, "verified_archive", return_value=({}, {"rows": "original"})
                ),
                patch.object(migration, "owned_container", return_value={"id": "1" * 64}),
                patch.object(migration, "no_clients"),
                patch.object(snapshot, "collect", return_value={"rows": "original"}),
                patch.object(migration, "record", side_effect=record),
                patch.object(migration, "stop_source_writers", side_effect=stop),
            ):
                if failed:
                    with self.assertRaisesRegex(release.ReleaseError, "shutdown_failed"):
                        _ = migration.seal(
                            Mock(spec=release.RunnerProtocol), request, {"phase": "frozen"}
                        )
                else:
                    _ = migration.seal(
                        Mock(spec=release.RunnerProtocol), request, {"phase": "frozen"}
                    )
            self.assertEqual(
                events, ["sealing", "stop-writers"] + ([] if failed else ["cutover-sealed"])
            )

    def test_source_writer_shutdown_checks_restart_policies_and_disabled_timers(self) -> None:
        """Successful commands alone cannot authorize startup while an old writer can restart."""
        name = "simplestchat-backup.timer"
        before: JsonObject = {name: {"loaded": True, "active": False, "enabled": "enabled"}}
        disabled: JsonObject = {name: {"loaded": True, "active": False, "enabled": "disabled"}}
        container: JsonObject = {"id": "1" * 64, "running": False, "restartPolicy": "no"}
        for unchanged in ("none", "timer", "restart", "running"):
            runner = Mock(spec=release.RunnerProtocol)
            actual = {
                **container,
                "running": unchanged == "running",
                "restartPolicy": "unless-stopped" if unchanged == "restart" else "no",
            }
            with (
                self.subTest(unchanged=unchanged),
                patch.object(
                    migration,
                    "units",
                    side_effect=[before, before if unchanged == "timer" else disabled],
                ),
                patch.object(migration, "owned_container", side_effect=[container, actual] * 3),
            ):
                if unchanged == "none":
                    migration.stop_source_writers(runner)
                else:
                    with self.assertRaises(release.ReleaseError):
                        migration.stop_source_writers(runner)
            self.assertEqual(
                cast("Mock", runner.run).call_args_list,
                [
                    call(["/usr/bin/systemctl", "stop", name], timeout=120),
                    call(["/usr/bin/systemctl", "disable", name], timeout=30),
                ],
            )
            self.assertEqual(
                cast("Mock", runner.docker).call_args_list[:2],
                [
                    call("update", "--restart=no", "1" * 64, timeout=15),
                    call("stop", "--time", "30", "1" * 64, timeout=45),
                ],
            )

    def test_partial_seal_can_finish_without_querying_a_stopped_database(self) -> None:
        """A shutdown retry never requires restarting the old database to prove the seal."""
        with (
            patch.object(migration, "verified_archive") as archive,
            patch.object(snapshot, "collect") as collect,
            patch.object(migration, "stop_source_writers") as stop,
            patch.object(migration, "record", return_value={"phase": "cutover-sealed"}),
        ):
            result = migration.seal(
                Mock(spec=release.RunnerProtocol),
                migration.parse_request(REQUEST),
                {"phase": "sealing"},
            )
        self.assertEqual(result["phase"], "cutover-sealed")
        stop.assert_called_once()
        archive.assert_not_called()
        collect.assert_not_called()


class MigrationPrivatePathsTests(unittest.TestCase):
    """Private evidence and shared journals bind one operation without following links."""

    def test_private_path_rejects_links_and_permissive_files(self) -> None:
        """Local fixtures never need root to prove the metadata policy."""
        with tempfile.TemporaryDirectory(dir=ROOT / "results") as directory:
            path = Path(directory) / "private.json"
            _ = path.write_text("{}")
            path.chmod(0o600)
            with patch.object(release, "ROOT_UID", os.geteuid()):
                migration.private(path)
                link = path.with_name("linked.json")
                link.symlink_to(path)
                with self.assertRaises(release.ReleaseError):
                    migration.private(link)
                link.unlink()
                os.link(path, link)
                with self.assertRaises(release.ReleaseError):
                    migration.private(path)
                link.unlink()
                path.chmod(0o644)
                with self.assertRaises(release.ReleaseError):
                    migration.private(path)

    def test_shared_journal_admits_only_its_bound_operation(self) -> None:
        """Another request cannot reuse the persistent migration ownership marker."""
        with tempfile.TemporaryDirectory(dir=ROOT / "results") as directory:
            root = Path(directory)
            config = root / "config"
            config.mkdir(mode=0o700)
            operation = root / "migrations" / ("a" * 32)
            operation.mkdir(mode=0o700, parents=True)
            with (
                patch.object(release, "ROOT", root),
                patch.object(release, "CONFIG", config),
                patch.object(release, "WORK", root / "work"),
                patch.object(release, "ROOT_UID", os.geteuid()),
            ):
                request = migration.parse_request(REQUEST)
                _ = migration.record(request, "frozen", {"role": "source"})
                with migration.workload(request):
                    self.assertEqual(object_value(migration.state(request))["phase"], "frozen")
                other = migration.parse_request({**REQUEST, "operationId": "c" * 32})
                with self.assertRaises(release.ReleaseError), migration.workload(other):
                    self.fail("foreign request entered the workload lock")
                _ = migration.record(request, "cutover-sealed", {"role": "source"})
                with self.assertRaises(release.ReleaseError):
                    _ = migration.recover(
                        Mock(spec=release.RunnerProtocol),
                        request,
                        object_value(migration.state(request)),
                    )

    def test_global_seal_blocks_recovery_after_local_journal_write_failure(self) -> None:
        """A durable global seal cannot be bypassed by the older local frozen phase."""
        with tempfile.TemporaryDirectory(dir=ROOT / "results") as directory:
            root = Path(directory)
            operation = root / "migrations" / ("a" * 32)
            operation.mkdir(mode=0o700, parents=True)
            with (
                patch.object(release, "ROOT", root),
                patch.object(release, "ROOT_UID", os.geteuid()),
            ):
                request = migration.parse_request(REQUEST)
                frozen = migration.record(request, "frozen", {"role": "source"})
                atomic = release.atomic

                def interrupted_write(path: Path, value: JsonObject) -> None:
                    if path == operation / "migration.json":
                        message = "simulated local journal write failure"
                        raise OSError(message)
                    atomic(path, value)

                with (
                    patch.object(release, "atomic", side_effect=interrupted_write),
                    self.assertRaises(OSError),
                ):
                    _ = migration.record(request, "sealing", frozen)
                self.assertEqual(object_value(migration.state(request))["phase"], "frozen")
                runner = Mock(spec=release.RunnerProtocol)
                with self.assertRaises(release.ReleaseError):
                    _ = migration.recover(runner, request, frozen)
                self.assertEqual(runner.mock_calls, [])


class MigrationStreamTests(unittest.TestCase):
    """Actual local pipes establish privacy, byte bounds, deadlines and cleanup."""

    def test_stream_hashes_complete_records_without_retaining_values(self) -> None:
        """The COPY protocol remains correct when output arrives in tiny chunks."""
        payload = b'{"private":"fixture-only"}\n{"number":2}\n'
        with tempfile.TemporaryDirectory(dir=ROOT / "results") as directory:
            adapter = Path(directory) / "client.py"
            _ = adapter.write_text(
                "import os,sys\n"
                + f"payload={payload!r}\n"
                + "for line in sys.stdin:\n"
                + " if line.startswith('\\\\quit'): break\n"
                + " if line.startswith('\\\\echo '):\n"
                + "  for byte in payload: os.write(1,bytes([byte]))\n"
                + "  os.write(1,line[6:].encode())\n"
            )
            with patch.object(release, "DOCKER", [sys.executable, str(adapter)]):
                session = snapshot.Session("1" * 64)
                try:
                    actual = session.digest("fixture statement;")
                    self.assertEqual(
                        actual,
                        {
                            "rows": 2,
                            "copyBytes": len(payload),
                            "sha256": hashlib.sha256(payload).hexdigest(),
                        },
                    )
                    self.assertNotIn("fixture-only", json.dumps(actual))
                finally:
                    session.close()
                self.assertIsNotNone(session.process.returncode)

    def test_quiet_client_hits_deadline_and_owned_process_is_reaped(self) -> None:
        """A silent client cannot leave an unbounded snapshot process behind."""
        with tempfile.TemporaryDirectory(dir=ROOT / "results") as directory:
            adapter = Path(directory) / "client.py"
            _ = adapter.write_text(
                "import sys\nfor line in sys.stdin:\n if line.startswith('\\\\quit'): break\n"
            )
            with (
                patch.object(release, "DOCKER", [sys.executable, str(adapter)]),
                patch.object(snapshot, "QUERY_SECONDS", 0.05),
            ):
                session = snapshot.Session("1" * 64)
                try:
                    with self.assertRaisesRegex(release.ReleaseError, "postgres_query_deadline"):
                        _ = session.digest("fixture statement;")
                finally:
                    session.close()
                self.assertIsNotNone(session.process.returncode)


@unittest.skipUnless(os.environ.get("DISPOSABLE_TEST_DATABASE") == "1", "requires owned PostgreSQL")
class MigrationDatabaseTests(unittest.TestCase):
    """Exercise real COPY streaming, PostgreSQL restoration and email preservation."""

    def command(self, name: str, *arguments: str, data: bytes | None = None) -> bytes:
        """Use only installed tools and the explicitly disposable fixture cluster."""
        executable = shutil.which(name)
        if executable is None:
            self.fail(name + " required")
        return subprocess.run(  # noqa: S603 -- fixed PostgreSQL tools and owned fixture URLs.
            [executable, *arguments], input=data, capture_output=True, check=True, timeout=40
        ).stdout

    def sql(self, url: str, source: str) -> bytes:
        """Execute fixture setup and assertions only inside owned fixture databases."""
        return self.command(
            "psql", "-X", "-qAt", "--set", "ON_ERROR_STOP=1", url, data=source.encode()
        )

    def snapshot(self, adapter: Path, url: str) -> JsonObject:
        """Drive the production stream protocol through a local fixture-only psql adapter."""
        with patch.object(release, "DOCKER", [sys.executable, str(adapter), url]):
            session = snapshot.Session("1" * 64)
            try:
                session.query(snapshot.SETUP, snapshot.no_output)
                catalog = object_value(session.value(snapshot.CATALOG_SQL))
                return {
                    "tables": snapshot.tables(session, catalog),
                    "sequences": snapshot.sequences(session, catalog),
                    "metadata": session.digest(snapshot.METADATA_SQL),
                }
            finally:
                session.close()

    def canonical_constraint(self, definition: str, kind: str = "c") -> str:
        """Evaluate the production SQL normalizer on synthetic catalog expressions only."""
        literal = "'" + definition.replace("'", "''") + "'"
        return (
            self.sql(
                os.environ["TEST_DATABASE_URL"],
                "BEGIN READ ONLY; SET search_path=pg_catalog; SELECT "  # noqa: S608 -- synthetic test literals, with SQL quote escaping.
                + snapshot.CONSTRAINT_DEFINITION_SQL
                + " FROM (VALUES ('"
                + kind
                + "')) c(contype)"
                + " CROSS JOIN (VALUES ("
                + literal
                + ")) d(definition); ROLLBACK;",
            )
            .decode()
            .removesuffix("\n")
        )

    def test_enum_check_normalization_preserves_every_other_expression(self) -> None:
        """Only the proven lossless array-cast relocation is canonicalized."""
        original = (
            "CHECK (kind::text = ANY (ARRAY['registration'::character varying, "
            + "'room'::character varying]::text[]))"
        )
        canonical = (
            "CHECK (kind::text = ANY (ARRAY['registration'::character varying::text, "
            + "'room'::character varying::text]))"
        )
        self.assertEqual(self.canonical_constraint(original), canonical)
        self.assertEqual(self.canonical_constraint(canonical), canonical)
        self.assertEqual(self.canonical_constraint(original, "f"), original)
        for expression in (
            original.replace("character varying", "character varying(1)"),
            original.replace(" = ANY ", " <> ALL "),
            original.replace("kind::text", "lower(kind::text)"),
            original.replace("'room'::character varying", "NULL::character varying"),
            original.replace("'room'", "'room''s'"),
            original.replace("'room'", "'room::character varying'"),
            original.replace("'room'::character varying", "room::character varying"),
            original + " NOT VALID",
            "prefix " + original,
        ):
            with self.subTest(expression=expression):
                self.assertEqual(self.canonical_constraint(expression), expression)
        for changed in (
            original.replace("kind::text", "other::text"),
            original.replace("'room'", "'private'"),
            original.replace("'registration'", "'room'").replace(
                "'room'::character varying]", "'registration'::character varying]"
            ),
        ):
            with self.subTest(changed=changed):
                self.assertNotEqual(self.canonical_constraint(changed), canonical)

    def test_real_restore_preserves_rows_partitions_acl_and_schema_digest(self) -> None:
        """Physical layout changes and dropped-column holes do not disguise logical drift."""
        base = os.environ["TEST_DATABASE_URL"]
        parsed = urlsplit(base)
        # The owner-update contract deliberately fixes the runtime database name.
        # CREATE must succeed before this fixture owns (and may later drop) it.
        names = ["migration_" + uuid.uuid4().hex, "simplestchat"]
        urls = [urlunsplit(parsed._replace(path="/" + name)) for name in names]
        _ = self.sql(
            base,
            """DO $$ BEGIN
IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname='postgres') THEN
 CREATE ROLE postgres NOLOGIN; END IF;
IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname='simplestchat_app') THEN
 CREATE ROLE simplestchat_app NOLOGIN; END IF;
IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname='simplestchat_migrate') THEN
 CREATE ROLE simplestchat_migrate NOLOGIN; END IF;
END $$;""",
        )
        created: list[str] = []
        try:
            for name in names:
                _ = self.sql(base, "CREATE DATABASE " + name + " OWNER postgres;")
                created.append(name)
            self.exercise_restore(urls[0], urls[1])
        finally:
            for name in created:
                _ = self.sql(base, "DROP DATABASE " + name + ";")

    def exercise_restore(self, source: str, destination: str) -> None:
        """Use synthetic records only, including newline data and partitioned storage."""
        _ = self.sql(
            source,
            """SET ROLE postgres;
CREATE SCHEMA operations;
REVOKE ALL ON SCHEMA operations FROM PUBLIC;
CREATE TABLE public.users(id uuid PRIMARY KEY,email text UNIQUE NOT NULL,password_hash text,
 auth_version bigint NOT NULL,removed text, payload jsonb,
 chat_style varchar(16) NOT NULL CONSTRAINT users_chat_style_value
 CHECK (chat_style IN ('accent','text','bubble')));
ALTER TABLE public.users DROP COLUMN removed;
CREATE TABLE public.rooms(id text PRIMARY KEY,owner_id uuid REFERENCES public.users(id));
CREATE TABLE public.partitioned(id int,payload text) PARTITION BY RANGE(id);
CREATE TABLE public.first_partition PARTITION OF public.partitioned FOR VALUES FROM(0) TO(10);
CREATE TABLE public.second_partition PARTITION OF public.partitioned FOR VALUES FROM(10) TO(20);
CREATE TABLE operations.events(id bigserial PRIMARY KEY,payload text);
GRANT SELECT,UPDATE ON public.users TO simplestchat_app;
INSERT INTO public.users VALUES('11111111-1111-4111-8111-111111111111',
 'owner@old.example.test','fixture-password-hash',9,'{"text":"line\\nnext\\tpart"}','accent');
INSERT INTO public.rooms VALUES('lobby','11111111-1111-4111-8111-111111111111');
INSERT INTO public.partitioned VALUES(1,E'a\\nb'),(11,'last');
INSERT INTO operations.events(payload) VALUES('fixture incident');""",
        )
        with tempfile.TemporaryDirectory(dir=ROOT / "results") as directory:
            path = Path(directory)
            adapter = path / "psql-adapter.py"
            psql = shutil.which("psql")
            self.assertIsNotNone(psql)
            _ = adapter.write_text(
                "import os,sys\n"
                + f"psql={psql!r}\n"
                + "os.execv(psql,[psql,'-X','-qAt','--set','ON_ERROR_STOP=on',sys.argv[1]])\n"
            )
            original = self.snapshot(adapter, source)
            table_rows = {
                str(object_value(item)["name"]): object_value(item)["rows"]
                for item in cast("list[JsonValue]", original["tables"])
            }
            self.assertEqual(table_rows["partitioned"], 0)
            self.assertEqual(table_rows["first_partition"], 1)
            self.assertEqual(table_rows["second_partition"], 1)
            self.assertNotIn("fixture-password-hash", json.dumps(original))
            archive = path / "database.dump"
            _ = self.command("pg_dump", "--format=custom", "--file", str(archive), source)
            _ = self.command(
                "pg_restore",
                "--exit-on-error",
                "--single-transaction",
                "--dbname",
                destination,
                str(archive),
            )
            restored = self.snapshot(adapter, destination)
            if restored != original:
                before_schema = self.sql(
                    source, "SET search_path=pg_catalog; " + snapshot.METADATA_SQL
                )
                after_schema = self.sql(
                    destination, "SET search_path=pg_catalog; " + snapshot.METADATA_SQL
                )
                difference = "\n".join(
                    difflib.unified_diff(
                        before_schema.decode().splitlines(), after_schema.decode().splitlines()
                    )
                )
                self.fail("Fixture restore differs: " + difference[:8192])
            self.verify_owner_update(adapter, destination, original)
            self.verify_enum_change(adapter, destination, original)
            _ = self.sql(destination, "UPDATE public.users SET auth_version=10;")
            self.assertNotEqual(self.snapshot(adapter, destination), original)
            _ = self.sql(destination, "UPDATE public.users SET auth_version=9;")
            _ = self.sql(destination, "GRANT DELETE ON public.users TO simplestchat_app;")
            changed = self.snapshot(adapter, destination)
            self.assertEqual(changed["tables"], original["tables"])
            self.assertNotEqual(changed["metadata"], original["metadata"])

    def verify_enum_change(self, adapter: Path, destination: str, original: JsonObject) -> None:
        """Detect a changed allowed enum value even when every stored row is identical."""
        for allowed in ("balloon", "bubble"):
            _ = self.sql(
                destination,
                "ALTER TABLE public.users DROP CONSTRAINT users_chat_style_value; "
                + "ALTER TABLE public.users ADD CONSTRAINT users_chat_style_value "
                + "CHECK (chat_style IN ('accent','text','"
                + allowed
                + "'));",
            )
            changed = self.snapshot(adapter, destination)
            self.assertEqual(changed["tables"], original["tables"])
            if allowed == "balloon":
                self.assertNotEqual(changed["metadata"], original["metadata"])
            else:
                self.assertEqual(changed, original)

    def verify_owner_update(self, adapter: Path, destination: str, original: JsonObject) -> None:
        """Run the real guarded transaction and compare its precise permitted row delta."""
        system_id = (
            self.sql(destination, "SELECT system_identifier FROM pg_control_system();")
            .decode()
            .strip()
        )
        _ = self.sql(destination, "GRANT EXECUTE ON FUNCTION pg_control_system() TO postgres;")
        with self.assertRaises(subprocess.CalledProcessError):
            _ = self.sql(
                destination,
                "BEGIN; SET ROLE postgres; "
                + migration.owner_sql("https://old.example.test", "https://new.example.test", "1")
                + " COMMIT;",
            )
        self.assertEqual(self.snapshot(adapter, destination), original)
        with patch.object(release, "DOCKER", [sys.executable, str(adapter), destination]):
            expected_users = snapshot.renamed_users(
                "1" * 64, "owner@old.example.test", "owner@new.example.test"
            )
        _ = self.sql(
            destination,
            "BEGIN; SET ROLE postgres; "
            + migration.owner_sql("https://old.example.test", "https://new.example.test", system_id)
            + " COMMIT;",
        )
        changed = self.snapshot(adapter, destination)
        for row in cast("list[JsonValue]", changed["tables"]):
            item = object_value(row)
            if item["name"] == "users" and item["schema"] == "public":
                self.assertEqual({key: item[key] for key in expected_users}, expected_users)
        self.assertEqual(changed["sequences"], original["sequences"])
        self.assertEqual(changed["metadata"], original["metadata"])
        self.assertEqual(
            self.sql(
                destination, "SELECT password_hash||':'||auth_version::text FROM public.users;"
            ),
            b"fixture-password-hash:9\n",
        )
        _ = self.sql(destination, "UPDATE public.users SET email='owner@old.example.test';")
        self.assertEqual(self.snapshot(adapter, destination), original)


if __name__ == "__main__":
    _ = unittest.main()
