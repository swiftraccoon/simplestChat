"""Migration preflight and cross-host failure boundaries using only local fixtures."""

import json
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from test_support import ROOT, array, objects, yaml_value

# isort: split
import migration_inspect as INSPECT  # noqa: N812 -- Name the standalone helper under test.


class MigrationInspectionTests(unittest.TestCase):
    """Keep existing host data and configuration outside the mutation path."""

    def test_only_a_target_accepts_preseeded_secrets_without_application_configuration(
        self,
    ) -> None:
        """Allow an absent target pair; refuse partial configuration or missing source files."""
        with tempfile.TemporaryDirectory(prefix="migration-inspect.") as temporary:
            root = Path(temporary)
            _ = (root / "secrets.json").write_text("fixture configuration")
            with patch.object(INSPECT, "CONFIG", root), patch.object(INSPECT, "protected"):
                result = INSPECT.configured("https://next.example.test", allow_unconfigured=True)
                self.assertEqual(result, {"domain": None, "rpId": None, "revision": None})
                with self.assertRaisesRegex(
                    INSPECT.InspectionError, "configuration_pair_incomplete"
                ):
                    _ = INSPECT.configured("https://next.example.test")
                _ = (root / "app.env").write_text("fixture incomplete configuration")
                with self.assertRaisesRegex(
                    INSPECT.InspectionError, "configuration_pair_incomplete"
                ):
                    _ = INSPECT.configured("https://next.example.test", allow_unconfigured=True)

    def test_configuration_returns_only_identity_and_requires_exact_origin(self) -> None:
        """Never return secret-bearing environment values or accept a different deployment."""
        with tempfile.TemporaryDirectory(prefix="migration-inspect.") as temporary:
            root = Path(temporary)
            environment = root / "app.env"
            original = (
                "ALLOWED_ORIGINS=https://chat.example.test\n"
                "WEBAUTHN_RP_ID=example.test\n"
                "PRIVATE_VALUE=NOT_FOR_OUTPUT\n"
            )
            _ = environment.write_text(original)
            _ = (root / "images.json").write_text(json.dumps({"revision": "a" * 40}))
            with patch.object(INSPECT, "CONFIG", root), patch.object(INSPECT, "protected"):
                value = INSPECT.configured("https://chat.example.test")
                self.assertEqual(
                    value,
                    {"domain": "chat.example.test", "rpId": "example.test", "revision": "a" * 40},
                )
                self.assertNotIn("NOT_FOR_OUTPUT", json.dumps(value))
                with self.assertRaisesRegex(INSPECT.InspectionError, "origin_mismatch"):
                    _ = INSPECT.configured("https://other.example.test")
                self.assertEqual(environment.read_text(), original)
                _ = environment.write_text(original + "WEBAUTHN_RP_ID=other.test\n")
                with self.assertRaisesRegex(INSPECT.InspectionError, "duplicate_environment_key"):
                    _ = INSPECT.configured("https://chat.example.test")

    def test_private_path_rejects_links_nonroot_and_readable_configuration(self) -> None:
        """File-kind and ownership checks run before secret configuration is read."""
        cases = [
            (stat.S_IFLNK | 0o600, 0),
            (stat.S_IFREG | 0o644, 0),
            (stat.S_IFREG | 0o600, 1000),
        ]
        for mode, owner in cases:
            with self.subTest(mode=mode, owner=owner):
                metadata = SimpleNamespace(st_mode=mode, st_uid=owner, st_size=100)
                with (
                    patch.object(Path, "lstat", return_value=metadata),
                    self.assertRaisesRegex(INSPECT.InspectionError, "unsafe_migration"),
                ):
                    INSPECT.protected(Path("fixture"))

    def test_existing_database_is_rejected_before_any_docker_call(self) -> None:
        """An existing destination cannot be repurposed merely because its app is stopped."""
        with tempfile.TemporaryDirectory(prefix="migration-inspect.") as temporary:
            root = Path(temporary)
            database = root / "postgres"
            database.mkdir()
            marker = database / "retained-data"
            _ = marker.write_text("preserve")
            metadata = SimpleNamespace(st_mode=stat.S_IFDIR | 0o700, st_uid=999, st_size=0)
            with (
                patch.object(INSPECT, "ROOT", root),
                patch.object(Path, "lstat", return_value=metadata),
                patch.object(subprocess, "run") as command,
            ):
                with self.assertRaisesRegex(
                    INSPECT.InspectionError, "destination_database_not_empty"
                ):
                    INSPECT.fresh_target()
                command.assert_not_called()
            self.assertEqual(marker.read_text(), "preserve")

    def test_fresh_debian_can_be_provisioned_without_preinstalled_docker(self) -> None:
        """A missing Docker executable is valid only after the database directory check."""
        with (
            tempfile.TemporaryDirectory(prefix="migration-inspect.") as temporary,
            patch.object(INSPECT, "ROOT", Path(temporary)),
            patch.object(shutil, "which", return_value=None),
            patch.object(subprocess, "run") as command,
        ):
            INSPECT.fresh_target()
            command.assert_not_called()

    def test_existing_container_or_failed_docker_inspection_is_rejected(self) -> None:
        """A stopped public container still makes the destination nonempty."""
        for code, output in [(0, "existing-container"), (1, "")]:
            with (
                tempfile.TemporaryDirectory(prefix="migration-inspect.") as temporary,
                patch.object(INSPECT, "ROOT", Path(temporary)),
                patch.object(shutil, "which", return_value="/usr/bin/docker"),
                patch.object(
                    subprocess, "run", return_value=subprocess.CompletedProcess([], code, output)
                ),
                self.assertRaisesRegex(
                    INSPECT.InspectionError, "destination_has_public_containers"
                ),
            ):
                INSPECT.fresh_target()

    def test_all_cross_host_play_failures_stop_before_later_host_actions(self) -> None:
        """A failed source seal must abort subsequent destination startup plays."""
        plays = objects(yaml_value((ROOT / "ops/ansible/migration-data.yml").read_text()))
        for play in plays:
            self.assertIs(play["any_errors_fatal"], expr2=True)
        commands = [
            array(task, "ansible.builtin.command", "argv")
            for play in plays
            for task in objects(play, "tasks")
            if "ansible.builtin.command" in task
        ]
        verification = next(index for index, argv in enumerate(commands) if "verify-target" in argv)
        seal = next(index for index, argv in enumerate(commands) if "seal-source" in argv)
        launch = next(
            index
            for index, argv in enumerate(commands)
            if "/usr/local/bin/simplestchat-public-deploy" in argv
        )
        self.assertLess(verification, seal)
        self.assertLess(seal, launch)
        for play in plays:
            for task in objects(play, "tasks"):
                if "ansible.builtin.copy" in task or "ansible.builtin.fetch" in task:
                    self.assertIs(task["no_log"], expr2=True)


if __name__ == "__main__":
    _ = unittest.main()
