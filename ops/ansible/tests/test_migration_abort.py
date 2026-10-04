"""A failed destination can be retained only before an application was created."""

import json
import os
import tempfile
import unittest
from pathlib import Path
from typing import cast
from unittest.mock import Mock, patch

from test_support import ROOT

# isort: split
import migrate_public as migration
import release_public as public
from release_json import JsonObject


class MigrationAbortTests(unittest.TestCase):
    """Exercise guarded lifecycle cleanup and actual preservation of the failed files."""

    def __init__(self, method_name: str = "runTest") -> None:
        """Give each test an explicit, isolated migration identity."""
        super().__init__(method_name)
        self.request: migration.Request = migration.Request(
            operation_id="a" * 32,
            source_origin="https://old.example.test",
            destination_origin="https://new.example.test",
            target_revision="b" * 40,
        )
        self.prior: JsonObject = {
            "role": "destination",
            "phase": "restoring",
            "before": {"configurationSha256": {"app.env": "original"}},
        }
        self.runner: Mock = Mock(spec=public.RunnerProtocol)

    def test_refuses_source_and_post_restore_phases_before_commands(self) -> None:
        """No recovery path may erase a destination that could have accepted writes."""
        for role, phase in (
            ("source", "restoring"),
            ("destination", "restored"),
            ("destination", "cutover-sealed"),
            ("destination", "aborted"),
        ):
            with self.subTest(role=role, phase=phase), self.assertRaises(public.ReleaseError):
                _ = migration.abort_target(
                    self.runner, self.request, {**self.prior, "role": role, "phase": phase}
                )
        self.assertEqual(self.runner.mock_calls, [])

    def test_even_stopped_application_container_blocks_abort(self) -> None:
        """A stopped app still proves that startup may have happened."""
        with (
            patch.object(migration, "read_object", return_value=self.shared()),
            patch.object(migration, "configuration_hashes", return_value={"app.env": "original"}),
            patch.object(migration, "owned_container", return_value={"running": False}),
            patch.object(migration, "record") as record,
            self.assertRaisesRegex(public.ReleaseError, "destination_application_was_created"),
        ):
            _ = migration.abort_target(self.runner, self.request, self.prior)
        record.assert_not_called()
        self.assertEqual(self.runner.mock_calls, [])

    def shared(self) -> JsonObject:
        """Return the exact unfinished journal paired with this local request."""
        return {
            "operation": "server_migration",
            "request": self.request.json(),
            "phase": "restoring",
            "finalized": False,
        }

    def test_mismatched_shared_journal_and_configuration_block_mutations(self) -> None:
        """An operator cannot abort another operation or a reconfigured deployment."""
        for shared, hashes in (
            ({**self.shared(), "phase": "restored"}, {"app.env": "original"}),
            ({**self.shared(), "finalized": True}, {"app.env": "original"}),
            ({**self.shared(), "request": {}}, {"app.env": "original"}),
            (self.shared(), {"app.env": "changed"}),
        ):
            with (
                self.subTest(shared=shared, hashes=hashes),
                patch.object(migration, "read_object", return_value=shared),
                patch.object(migration, "configuration_hashes", return_value=hashes),
                self.assertRaises(public.ReleaseError),
            ):
                _ = migration.abort_target(self.runner, self.request, self.prior)
        self.assertEqual(self.runner.mock_calls, [])

    def test_abort_removes_only_owned_container_and_preserves_database(self) -> None:
        """Journal the abort before stopping PostgreSQL; retain its bind mount before finalizing."""
        container: JsonObject = {"id": "1" * 64, "running": True}
        mounts = [
            {
                "Type": "bind",
                "Source": str(public.ROOT / "postgres"),
                "Destination": "/var/lib/postgresql",
            }
        ]
        docker = cast("Mock", self.runner.docker)
        docker.return_value = json.dumps(mounts).encode()
        with (
            patch.object(migration, "read_object", return_value=self.shared()),
            patch.object(migration, "configuration_hashes", return_value={"app.env": "original"}),
            patch.object(migration, "owned_container", side_effect=[None, None, container]),
            patch.object(migration, "record") as record,
            patch.object(migration, "retain_failed_database") as retain,
        ):
            _ = migration.abort_target(self.runner, self.request, self.prior)
        phases = [item.args[1] for item in record.call_args_list]
        self.assertEqual(phases, ["aborting", "aborted"])
        self.assertTrue(record.call_args_list[-1].kwargs["finalized"])
        docker.assert_any_call("stop", "--time", "30", "1" * 64, timeout=45)
        docker.assert_any_call("rm", "1" * 64, timeout=15)
        retain.assert_called_once_with(self.request)
        self.assertFalse(any("--volumes" in item.args for item in docker.call_args_list))

    def test_retains_files_and_repeated_completion_never_overwrites_replacement(self) -> None:
        """Real directory moves keep the failed database bytes and reject a new nonempty one."""
        with (
            tempfile.TemporaryDirectory(dir=ROOT) as name,
            patch.object(public, "ROOT", Path(name)),
            patch.object(migration, "POSTGRES_UID", os.getuid()),
            patch.object(os, "chown") as chown,
        ):
            self.request.directory.mkdir(parents=True, mode=0o700)
            database = Path(name) / "postgres"
            database.mkdir(mode=0o700)
            _ = (database / "preserved").write_bytes(b"failed database bytes")
            migration.retain_failed_database(self.request)
            self.assertEqual(
                (self.request.directory / "retained-postgres/preserved").read_bytes(),
                b"failed database bytes",
            )
            self.assertEqual(list(database.iterdir()), [])
            chown.assert_called_once()
            migration.retain_failed_database(self.request)
            _ = (database / "new").write_bytes(b"replacement data")
            with self.assertRaisesRegex(public.ReleaseError, "replacement_database_not_empty"):
                migration.retain_failed_database(self.request)
            self.assertEqual((database / "new").read_bytes(), b"replacement data")

    def test_symlink_database_is_never_followed(self) -> None:
        """An unexpected filesystem link cannot move another database."""
        with (
            tempfile.TemporaryDirectory(dir=ROOT) as name,
            patch.object(public, "ROOT", Path(name)),
            patch.object(migration, "POSTGRES_UID", os.getuid()),
        ):
            self.request.directory.mkdir(parents=True, mode=0o700)
            elsewhere = Path(name) / "elsewhere"
            elsewhere.mkdir(mode=0o700)
            (Path(name) / "postgres").symlink_to(elsewhere, target_is_directory=True)
            with self.assertRaisesRegex(public.ReleaseError, "unsafe_failed_database_directory"):
                migration.retain_failed_database(self.request)
            self.assertTrue(elsewhere.is_dir())


if __name__ == "__main__":
    _ = unittest.main()
