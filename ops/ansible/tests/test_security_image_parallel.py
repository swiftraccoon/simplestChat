"""Prove fresh database and offline artifact checks overlap without shared lifecycles."""

from __future__ import annotations

import os
import signal
import tempfile
import threading
import time
import unittest
from pathlib import Path
from typing import TYPE_CHECKING
from unittest.mock import patch

from test_support import ROOT

# isort: split
import release_build
import security_image as image
import security_image_policy as policy
import security_secret_projection as projection
from release_json import object_value
from security_tools import ToolError
from test_security_image_runner import BASE, IDENTIFIER, Engine

if TYPE_CHECKING:
    from collections.abc import Sequence

    from release_json import JsonObject

_ = ROOT


def sandbox(root: Path) -> image.Sandbox:
    """Give the main branch its own command ledger, as in the actual controller."""
    logs = root / "scanner-commands"
    logs.mkdir()
    return image.Sandbox(
        ["/usr/bin/docker"],
        {},
        root,
        root / "tools",
        BASE,
        "linux/amd64",
        runner=release_build.Runner(output=logs),
    )


class ParallelImageTests(unittest.TestCase):
    """Database freshness, checked outputs and failure evidence remain mandatory."""

    def test_two_fresh_branches_join_before_matching_and_keep_unique_diagnostics(self) -> None:
        """All seven scanner lifecycles remain present, with independent bounded ledgers."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            selected = sandbox(root)
            engines: dict[int, Engine] = {}
            barrier = threading.Barrier(2, timeout=2)
            finished: list[str] = []

            def command(
                current: image.Sandbox, argv: Sequence[str], **_kwargs: object
            ) -> tuple[int, str]:
                if "sbom:/input/sbom.syft.json" in argv:
                    self.assertCountEqual(finished, ["database", "artifacts"])
                return engines.setdefault(id(current), Engine()).command(argv)

            def database(current: image.Sandbox) -> JsonObject:
                self.assertIsNot(current.runner, selected.runner)
                self.assertNotEqual(current.runner.output, selected.runner.output)
                _ = barrier.wait()
                _ = current.run("grype", ["db", "update"], destination=root / "db", online=True)
                _ = current.run("grype", ["db", "status"], destination=root / "status")
                finished.append("database")
                return {"fresh": True}

            def artifacts(current: image.Sandbox, *_args: object) -> tuple[Path, Path]:
                _ = barrier.wait()
                for tool, name in (("syft", "sbom"), ("syft", "spdx"), ("gitleaks", "secrets")):
                    _ = current.run(tool, ["scan"], destination=root / name)
                finished.append("artifacts")
                return root / "sbom", root / "secrets"

            with (
                patch.object(image.Sandbox, "command", command),
                patch.object(image, "prepare_database", side_effect=database) as prepare,
                patch.object(image, "scan_artifacts", side_effect=artifacts),
            ):
                _ = selected.run("gitleaks", ["self-test"], destination=root / "canary")
                result = image.scan_reports(selected, image.Options(), root / "tree", {}, {})
            prepare.assert_called_once()
            self.assertEqual(result[-1], {"fresh": True})
            receipts = list(root.glob("scanner-result-*.json"))
            self.assertEqual(len(receipts), 9)  # Seven containers and two phase summaries.
            self.assertEqual(len(list(root.glob("scanner-result-database-*.json"))), 2)
            self.assertEqual(selected.sequence, 5)
            self.assertEqual(selected.scanner_limit + 2, image.MAX_SCANNERS)
            for path in receipts:
                receipt = object_value(policy.report(path))
                self.assertIn("elapsedSeconds", receipt)
                if "tool" in receipt:
                    self.assertTrue(receipt["cleanupVerified"])
            self.assertEqual(sum(len(e.calls) for e in engines.values()), 7 * 4)

    def test_both_independent_failures_survive_and_no_matcher_starts(self) -> None:
        """Neither branch failure hides the other or permits partial policy evaluation."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            selected = sandbox(root)
            barrier = threading.Barrier(2, timeout=2)

            def fail_database(_current: image.Sandbox) -> JsonObject:
                _ = barrier.wait()
                reason = "image_database_update_failed"
                raise ToolError(reason)

            def fail_artifacts(_current: image.Sandbox, *_args: object) -> tuple[Path, Path]:
                _ = barrier.wait()
                reason = "image_secret_scan_failed"
                raise ToolError(reason)

            with (
                patch.object(image, "prepare_database", side_effect=fail_database),
                patch.object(image, "scan_artifacts", side_effect=fail_artifacts),
                patch.object(selected, "run") as run,
                self.assertRaisesRegex(ToolError, "image_parallel_scan_failed"),
            ):
                _ = image.scan_reports(selected, image.Options(), root / "tree", {}, {})
            run.assert_not_called()
            for name, failure in (
                ("database", "image_database_update_failed"),
                ("artifacts", "image_secret_scan_failed"),
            ):
                receipt = object_value(policy.report(root / f"scanner-result-phase-{name}.json"))
                self.assertFalse(receipt["passed"])
                self.assertEqual(receipt["failure"], failure)

    def test_signal_cancels_both_branches_and_keeps_exact_cleanup(self) -> None:
        """An actual SIGTERM reaches both health callbacks and joins cleanup before returning."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            selected = sandbox(root)
            engines: dict[int, Engine] = {}
            barrier = threading.Barrier(3, timeout=3)
            previous = signal.getsignal(signal.SIGTERM)

            def command(
                current: image.Sandbox, argv: Sequence[str], **_kwargs: object
            ) -> tuple[int, str]:
                result = engines.setdefault(id(current), Engine()).command(argv)
                if argv[0] == "run":
                    _ = barrier.wait()
                    deadline = time.monotonic() + 3
                    while current.runner.healthy is not None and current.runner.healthy():
                        if time.monotonic() >= deadline:
                            reason = "cancellation_not_observed"
                            raise AssertionError(reason)
                        time.sleep(0.01)
                    reason = "owned command cancelled"
                    raise release_build.BuildError(reason)
                return result

            def database(current: image.Sandbox) -> JsonObject:
                _ = current.run("grype", ["db", "update"], destination=root / "db", online=True)
                return {}

            def artifacts(current: image.Sandbox, *_args: object) -> tuple[Path, Path]:
                _ = current.run("syft", ["scan"], destination=root / "sbom")
                return root / "sbom", root / "secrets"

            def interrupt() -> None:
                _ = barrier.wait()
                os.kill(os.getpid(), signal.SIGTERM)

            interrupter = threading.Thread(target=interrupt)
            interrupter.start()
            try:
                with (
                    patch.object(image.Sandbox, "command", command),
                    patch.object(image, "prepare_database", side_effect=database),
                    patch.object(image, "scan_artifacts", side_effect=artifacts),
                    self.assertRaisesRegex(ToolError, "image_scan_cancelled"),
                ):
                    _ = image.scan_reports(selected, image.Options(), root / "tree", {}, {})
            finally:
                interrupter.join(timeout=4)
            self.assertFalse(interrupter.is_alive())
            self.assertEqual(signal.getsignal(signal.SIGTERM), previous)
            self.assertEqual(len(engines), 2)
            for engine in engines.values():
                self.assertIn(["container", "rm", "--force", IDENTIFIER], engine.calls)
            for name in ("database", "artifacts"):
                receipt = object_value(policy.report(root / f"scanner-result-phase-{name}.json"))
                self.assertFalse(receipt["passed"])
                self.assertTrue(receipt["cancelled"])

    def test_projection_observes_cancellation_between_files(self) -> None:
        """A cancelled branch cannot continue CPU/disk projection before noticing its event."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            layers = root / "layers"
            layers.mkdir()
            _ = (layers / "a").write_bytes(b"first")
            _ = (layers / "b").write_bytes(b"second")
            cancelled = threading.Event()
            project = projection.project

            def project_and_cancel(
                source: Path, destination: Path, *, max_bytes: int
            ) -> projection.Projection:
                result = project(source, destination, max_bytes=max_bytes)
                cancelled.set()
                return result

            with (
                patch.object(projection, "project", side_effect=project_and_cancel) as invoke,
                self.assertRaisesRegex(ToolError, "image_scan_cancelled"),
            ):
                _ = image.secret_bundle(layers, root, cancelled=cancelled)
            self.assertEqual(invoke.call_count, 1)
            self.assertFalse((root / "secret-paths.json").exists())


if __name__ == "__main__":
    _ = unittest.main()
