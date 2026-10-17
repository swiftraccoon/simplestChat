"""Check early database preparation without image artifacts or a real container engine."""

from __future__ import annotations

import json
import signal
import tempfile
import unittest
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import TYPE_CHECKING
from unittest.mock import patch

from test_support import ROOT

# isort: split
import security_image as image
import security_image_cache as cache
import security_image_policy as policy
import security_tools
from release_json import object_value
from security_tools import ToolError
from test_security_image_runner import BASE, Engine

if TYPE_CHECKING:
    from collections.abc import Sequence

    from release_json import JsonObject


class DatabasePreparationTests(unittest.TestCase):
    """The early job publishes data only after fresh validation and owned cleanup."""

    def prepare(self, root: Path, *, failure: str = "") -> tuple[bool, Engine]:
        """Exercise the real sandbox, phase ledger and cache publication with inert bytes."""
        engine = Engine(changed=failure == "cleanup")
        output, saved = root / "preparation", root / "saved"
        tools = output / "tools"
        status: JsonObject = {
            "valid": True,
            "schemaVersion": "v6.1.10",
            "from": "https://grype.anchore.io/databases/v6/fixture.tar.zst?checksum=sha256%3A"
            + "a" * 64,
            "built": (
                datetime.now(UTC) - timedelta(hours=121 if failure == "stale" else 1)
            ).isoformat(),
        }

        def command(
            _sandbox: image.Sandbox,
            argv: Sequence[str],
            *,
            timeout: int = 30,
            allow_failure: bool = False,
        ) -> tuple[int, str]:
            if list(argv[:1]) == ["pull"]:
                return 0, ""
            if list(argv[:2]) == ["image", "inspect"]:
                return 0, BASE
            result = engine.command(argv, timeout=timeout, allow_failure=allow_failure)
            if argv[0] != "run":
                return result
            if list(argv[-2:]) == ["db", "update"]:
                database = output / "database/cache/6"
                database.mkdir(parents=True)
                _ = (database / "vulnerability.db").write_bytes(b"inert checked database")
                _ = (database / "import.json").write_bytes(b'{"checksum":"fixture"}')
                if failure == "cancel":
                    signal.raise_signal(signal.SIGTERM)
                return (1, "") if failure == "update" else (0, "")
            if list(argv[-4:]) == ["db", "status", "-o", "json"]:
                return (1, "") if failure == "status" else (0, json.dumps(status))
            raise AssertionError(argv)

        def install_checked(
            _names: Sequence[str], directory: Path, *, target_platform: str
        ) -> Path:
            security_tools.private_directory(directory.parent)
            self.assertEqual(target_platform, "linux-x86_64")
            return directory

        with (
            patch.object(security_tools, "install", side_effect=install_checked) as install,
            patch.object(image, "tool_bundle", return_value=tools),
            patch.object(image.Sandbox, "command", new=command),
            patch.object(cache, "save", wraps=cache.save) as save,
        ):
            previous = signal.getsignal(signal.SIGTERM)
            passed = image.execute_database(
                image.Options(output=output, tools_directory=tools, database_cache=saved)
            )
            self.assertEqual(signal.getsignal(signal.SIGTERM), previous)
            install.assert_called_once_with(image.TOOLS, tools, target_platform="linux-x86_64")
            self.assertEqual(save.call_count, int(passed))
        evidence = object_value(policy.report(output / "database-preparation.json"))
        self.assertEqual(evidence["passed"], passed)
        self.assertEqual(evidence["cancelled"], failure in {"cancel", "cleanup"})
        if passed:
            self.assertEqual(
                evidence["databaseEvidenceSha256"], image.digest(output / "database-status.json")
            )
            self.assertTrue((saved / "receipt.json").is_file())
            self.assertNotIn("imageId", evidence)
            self.assertNotIn("checks", evidence)
        else:
            self.assertFalse(saved.exists())
            self.assertIsInstance(evidence["error"], str)
        return passed, engine

    def test_early_preparation_updates_online_and_publishes_only_checked_data(self) -> None:
        """The updater has no artifact mount; offline status and exact cleanup precede save."""
        with tempfile.TemporaryDirectory(dir=ROOT / "results") as temporary:
            passed, engine = self.prepare(Path(temporary))
            self.assertTrue(passed)
            scans = [call for call in engine.calls if call[0] == "run"]
            self.assertEqual(len(scans), 2)
            self.assertEqual(
                [call[call.index("--network") + 1] for call in scans], ["bridge", "none"]
            )
            self.assertFalse(
                any(
                    "dst=/input" in argument or "dst=/layers" in argument
                    for call in scans
                    for argument in call
                )
            )
            self.assertEqual(
                len([call for call in engine.calls if call[:3] == ["container", "rm", "--force"]]),
                2,
            )

    def test_failure_stale_data_cancellation_and_ambiguous_cleanup_never_publish(self) -> None:
        """All incomplete preparations retain failed evidence without a reusable cache."""
        for failure in ("update", "status", "stale", "cancel", "cleanup"):
            with (
                self.subTest(failure=failure),
                tempfile.TemporaryDirectory(dir=ROOT / "results") as temporary,
            ):
                passed, _ = self.prepare(Path(temporary), failure=failure)
                self.assertFalse(passed)

    def test_install_failure_retains_failure_evidence_and_never_opens_an_engine(self) -> None:
        """Authentication failure cannot be treated as a harmless warm-cache miss."""
        with tempfile.TemporaryDirectory(dir=ROOT / "results") as temporary:
            root = Path(temporary)
            output = root / "out"
            with (
                patch.object(
                    security_tools, "install", side_effect=ToolError("fixture_install_failed")
                ),
                patch.object(image, "prepare_sandbox") as prepare,
            ):
                self.assertFalse(
                    image.execute_database(
                        image.Options(
                            output=output,
                            tools_directory=root / "tools",
                            database_cache=root / "saved",
                        )
                    )
                )
            prepare.assert_not_called()
            self.assertEqual(
                object_value(policy.report(output / "database-preparation.json"))["error"],
                "fixture_install_failed",
            )
