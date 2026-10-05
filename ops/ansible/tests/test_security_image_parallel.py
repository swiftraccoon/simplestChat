"""Prove three independent image branches overlap within two owned scanner slots."""

from __future__ import annotations

import os
import signal
import tempfile
import threading
import time
import unittest
from concurrent.futures import ThreadPoolExecutor
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

    def assert_independent_ledgers(
        self, root: Path, selected: image.Sandbox, branches: list[image.Sandbox]
    ) -> None:
        """Check complete unique evidence and partitioned invocation/resource budgets."""
        receipts = list(root.glob("scanner-result-*.json"))
        self.assertEqual(len(receipts), 10)  # Seven containers and three phase summaries.
        for name, count in (("database", 2), ("sbom", 2), ("secrets", 1)):
            self.assertEqual(len(list(root.glob(f"scanner-result-{name}-*.json"))), count)
        self.assertEqual(selected.sequence, 2)
        self.assertEqual(
            selected.scanner_limit + sum(branch.scanner_limit for branch in branches),
            image.MAX_SCANNERS,
        )
        self.assertEqual(len({id(branch.runner) for branch in [selected, *branches]}), 4)
        self.assertEqual(len({branch.runner.output for branch in [selected, *branches]}), 4)
        for branch in branches:
            self.assertIs(branch.scanner_slots, selected.scanner_slots)
            self.assertIs(branch.cancelled, selected.cancelled)
        for path in receipts:
            receipt = object_value(policy.report(path))
            self.assertIn("elapsedSeconds", receipt)
            if "tool" in receipt:
                self.assertTrue(receipt["cleanupVerified"])

    def assert_cancelled_phases(self, root: Path) -> None:
        """Every running or queued branch records the shared cancellation."""
        for name in ("database", "sbom", "secrets"):
            receipt = object_value(policy.report(root / f"scanner-result-phase-{name}.json"))
            self.assertFalse(receipt["passed"])
            self.assertTrue(receipt["cancelled"])

    def test_three_branches_overlap_and_join_with_independent_bounded_ledgers(self) -> None:
        """All seven scanner lifecycles remain present, with independent bounded ledgers."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            selected = sandbox(root)
            engines: dict[int, Engine] = {}
            barrier = threading.Barrier(3, timeout=3)
            artifact_overlap = threading.Barrier(2, timeout=3)
            finished: list[str] = []
            branches: list[image.Sandbox] = []
            active: set[int] = set()
            lock = threading.Lock()

            def command(
                current: image.Sandbox, argv: Sequence[str], **_kwargs: object
            ) -> tuple[int, str]:
                if "sbom:/input/sbom.syft.json" in argv:
                    self.assertCountEqual(finished, ["database", "sbom", "secrets"])
                if argv[0] == "run":
                    with lock:
                        active.add(id(current))
                        self.assertLessEqual(len(active), image.MAX_ACTIVE_SCANNERS)
                result = engines.setdefault(id(current), Engine()).command(argv)
                if argv[0] == "run" and argv[-1] in {"catalog", "secret-scan"}:
                    _ = artifact_overlap.wait()
                if argv[0] == "ps":
                    with lock:
                        active.remove(id(current))
                return result

            def database(current: image.Sandbox) -> JsonObject:
                branches.append(current)
                _ = barrier.wait()
                _ = current.run("grype", ["db", "update"], destination=root / "db", online=True)
                _ = current.run("grype", ["db", "status"], destination=root / "status")
                finished.append("database")
                return {"fresh": True}

            def sbom(current: image.Sandbox, *_args: object) -> Path:
                branches.append(current)
                _ = barrier.wait()
                _ = current.run("syft", ["catalog"], destination=root / "sbom")
                _ = current.run("syft", ["convert"], destination=root / "spdx")
                finished.append("sbom")
                return root / "sbom"

            def secrets(current: image.Sandbox, *_args: object) -> Path:
                branches.append(current)
                _ = barrier.wait()
                _ = current.run("gitleaks", ["secret-scan"], destination=root / "secrets")
                finished.append("secrets")
                return root / "secrets"

            with (
                patch.object(image.Sandbox, "command", command),
                patch.object(image, "prepare_database", side_effect=database) as prepare,
                patch.object(image, "scan_sbom", side_effect=sbom),
                patch.object(image, "scan_secrets", side_effect=secrets),
            ):
                _ = selected.run("gitleaks", ["self-test"], destination=root / "canary")
                result = image.scan_reports(selected, image.Options(), root / "tree", {}, {})
            prepare.assert_called_once()
            self.assertEqual(result[-1], {"fresh": True})
            self.assert_independent_ledgers(root, selected, branches)
            self.assertEqual(sum(len(e.calls) for e in engines.values()), 7 * 4)
            self.assertFalse(active)

    def test_all_independent_failures_survive_and_no_matcher_starts(self) -> None:
        """No branch failure hides another or permits partial policy evaluation."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            selected = sandbox(root)
            barrier = threading.Barrier(3, timeout=3)

            def fail_database(_current: image.Sandbox) -> JsonObject:
                _ = barrier.wait()
                reason = "image_database_update_failed"
                raise ToolError(reason)

            def fail_sbom(_current: image.Sandbox, *_args: object) -> Path:
                _ = barrier.wait()
                reason = "image_sbom_failed"
                raise ToolError(reason)

            def fail_secrets(_current: image.Sandbox, *_args: object) -> Path:
                _ = barrier.wait()
                reason = "image_secret_scan_failed"
                raise ToolError(reason)

            with (
                patch.object(image, "prepare_database", side_effect=fail_database),
                patch.object(image, "scan_sbom", side_effect=fail_sbom),
                patch.object(image, "scan_secrets", side_effect=fail_secrets),
                patch.object(selected, "run") as run,
                self.assertRaisesRegex(ToolError, "image_parallel_scan_failed"),
            ):
                _ = image.scan_reports(selected, image.Options(), root / "tree", {}, {})
            run.assert_not_called()
            for name, failure in (
                ("database", "image_database_update_failed"),
                ("sbom", "image_sbom_failed"),
                ("secrets", "image_secret_scan_failed"),
            ):
                receipt = object_value(policy.report(root / f"scanner-result-phase-{name}.json"))
                self.assertFalse(receipt["passed"])
                self.assertEqual(receipt["failure"], failure)
                self.assertFalse(receipt["cancelled"])

    def test_signal_cancels_active_and_waiting_branches_with_exact_cleanup(self) -> None:
        """SIGTERM reaches active callbacks and the queued slot without starting a third scan."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            selected = sandbox(root)
            engines: dict[int, Engine] = {}
            barrier = threading.Barrier(3, timeout=3)
            phases = threading.Barrier(3, timeout=3)
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
                _ = phases.wait()
                _ = current.run("grype", ["db", "update"], destination=root / "db", online=True)
                return {}

            def sbom(current: image.Sandbox, *_args: object) -> Path:
                _ = phases.wait()
                _ = current.run("syft", ["scan"], destination=root / "sbom")
                return root / "sbom"

            def secrets(current: image.Sandbox, *_args: object) -> Path:
                _ = phases.wait()
                _ = current.run("gitleaks", ["scan"], destination=root / "secrets")
                return root / "secrets"

            def interrupt() -> None:
                _ = barrier.wait()
                os.kill(os.getpid(), signal.SIGTERM)

            interrupter = threading.Thread(target=interrupt)
            interrupter.start()
            try:
                with (
                    patch.object(image.Sandbox, "command", command),
                    patch.object(image, "prepare_database", side_effect=database),
                    patch.object(image, "scan_sbom", side_effect=sbom),
                    patch.object(image, "scan_secrets", side_effect=secrets),
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
            self.assert_cancelled_phases(root)

    def test_unverified_cleanup_cancels_waiter_before_releasing_scanner_slot(self) -> None:
        """A possibly surviving container cannot be followed by another queued scanner."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            first = sandbox(root)
            first.scanner_slots = threading.BoundedSemaphore(1)
            waiter = image.scan_branch(first, "waiter", 1)
            engine = Engine(changed=True)
            running = threading.Event()
            queued = threading.Event()
            release = threading.Event()

            def command(
                current: image.Sandbox, argv: Sequence[str], **_kwargs: object
            ) -> tuple[int, str]:
                self.assertIs(current, first)
                if argv[0] == "run":
                    running.set()
                    self.assertTrue(release.wait(timeout=3))
                return engine.command(argv)

            def wait_for_slot() -> tuple[int, str]:
                queued.set()
                return waiter.run("syft", ["scan"], destination=root / "queued")

            with patch.object(image.Sandbox, "command", command), ThreadPoolExecutor(2) as workers:
                first_future = workers.submit(
                    first.run, "grype", ["db", "update"], destination=root / "db", online=True
                )
                try:
                    self.assertTrue(running.wait(timeout=3))
                    waiter_future = workers.submit(wait_for_slot)
                    self.assertTrue(queued.wait(timeout=3))
                finally:
                    release.set()
                with self.assertRaisesRegex(ToolError, "image_container_identity_changed"):
                    _ = first_future.result(timeout=3)
                with self.assertRaisesRegex(ToolError, "image_scan_cancelled"):
                    _ = waiter_future.result(timeout=3)
            self.assertEqual(sum(call[0] == "run" for call in engine.calls), 1)
            self.assertFalse(any(call[:2] == ["container", "rm"] for call in engine.calls))
            self.assertEqual(waiter.sequence, 0)
            self.assertFalse((root / "queued").exists())
            self.assertTrue(first.scanner_slots.acquire(blocking=False))
            first.scanner_slots.release()
            receipt = object_value(policy.report(root / "scanner-result-01.json"))
            self.assertFalse(receipt["cleanupVerified"])

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
