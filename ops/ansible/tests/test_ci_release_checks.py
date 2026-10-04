"""Exercise bounded release/tool overlap without Docker or external downloads."""

from __future__ import annotations

import os
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

from test_support import ROOT

# isort: split
import ci_release_checks as checks
import security_check
from security_context import Context
from security_tools import ToolError


def task(name: str, program: str, *arguments: str, timeout: float = 3) -> checks.Task:
    """Run only the current interpreter with a test-owned inert program."""
    return checks.Task(name, (sys.executable, "-c", program, *arguments), timeout)


class ReleaseOverlapTests(unittest.TestCase):
    """Each result stays binding while work actually overlaps and children are reaped."""

    def test_both_children_start_before_either_finishes(self) -> None:
        """A cross-process barrier fails if the two independent commands become serial."""
        program = (
            "import pathlib,sys,time; own,other=map(pathlib.Path,sys.argv[1:]); "
            + "own.touch(); deadline=time.monotonic()+2\n"
            + "while not other.exists() and time.monotonic()<deadline: time.sleep(.01)\n"
            + "sys.exit(0 if other.exists() else 42)"
        )
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            first, second = output / "a.ready", output / "b.ready"
            results = checks.run_tasks(
                (
                    task("first", program, str(first), str(second)),
                    task("second", program, str(second), str(first)),
                ),
                output,
                threading.Event(),
            )
            self.assertEqual([result.status for result in results], [0, 0])
            for path in output.glob("*.std*"):
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_both_failure_statuses_and_private_logs_are_retained(self) -> None:
        """The first failure cannot cancel or hide the independent second failure."""
        program = (
            "import sys,time; time.sleep(float(sys.argv[1])); "
            + "print('private'); sys.exit(int(sys.argv[2]))"
        )
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            results = checks.run_tasks(
                (task("first", program, "0", "17"), task("second", program, ".1", "23")),
                output,
                threading.Event(),
            )
            self.assertEqual([result.status for result in results], [17, 23])
            self.assertEqual([result.failure for result in results], [None, None])
            for name in ("first", "second"):
                self.assertEqual((output / f"{name}.stdout").read_text(), "private\n")

    def test_timeout_and_cancellation_reap_children(self) -> None:
        """Owned children cannot continue after a bound or parent cancellation ends the run."""
        program = (
            "import os,pathlib,sys,time; pathlib.Path(sys.argv[1]).write_text(str(os.getpid())); "
            + "time.sleep(30)"
        )
        for cancel in (False, True):
            with self.subTest(cancel=cancel), tempfile.TemporaryDirectory() as temporary:
                output = Path(temporary)
                event = threading.Event()
                timer = threading.Timer(0.3, event.set)
                if cancel:
                    timer.start()
                try:
                    results = checks.run_tasks(
                        (
                            task("first", program, str(output / "first.pid"), timeout=0.5),
                            task("second", program, str(output / "second.pid"), timeout=0.5),
                        ),
                        output,
                        event,
                    )
                finally:
                    timer.cancel()
                    if cancel:
                        timer.join()
                expected = "command_lease_lost" if cancel else "command_timed_out"
                self.assertEqual([result.failure for result in results], [expected, expected])
                for name in ("first", "second"):
                    pid = int((output / f"{name}.pid").read_text())
                    with self.assertRaises(ProcessLookupError):
                        os.kill(pid, 0)

    def test_only_fixture_command_can_use_the_engine(self) -> None:
        """Prep is the pinned download-only installer, never a concurrent image scanner."""
        output = ROOT / "results/unused-release-test"
        with patch.object(checks, "executable", return_value="/usr/bin/sudo"):
            fixture, tools = checks.tasks(
                Path(sys.executable), "sha256:" + "a" * 64, output, output
            )
        self.assertEqual(
            fixture.command[:4],
            (
                "/usr/bin/sudo",
                sys.executable,
                "-B",
                "build/test-release-container.py",
            ),
        )
        self.assertIn("--disposable-host", fixture.command)
        self.assertEqual(fixture.timeout, 590)
        self.assertEqual(
            tools.command,
            (
                sys.executable,
                "-B",
                "build/security_tools.py",
                "install",
                "--tools",
                "syft",
                "grype",
                "gitleaks",
                "--platform",
                "linux-x86_64",
                "--directory",
                str(output / "tools"),
            ),
        )
        self.assertEqual(tools.timeout, 300)


class PreparedImageToolTests(unittest.TestCase):
    """A prepared directory never substitutes for authenticated tool verification."""

    def test_prepared_tools_are_reverified_before_the_same_image_scan(self) -> None:
        """Reuse runs the existing installer validation before starting any scanner."""
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            context = Context(ROOT, output)
            tools = output / "tools"
            tools.mkdir()
            options = security_check.Options(
                tier="image",
                image="sha256:" + "a" * 64,
                artifact_dir=output / "export",
                image_tools=tools,
            )
            with (
                patch.object(security_check, "install", return_value=tools) as install,
                patch.object(Context, "run", return_value=(0, b"")) as run,
            ):
                security_check.image(context, options)
                install.assert_called_once_with(
                    ["syft", "grype", "gitleaks"], tools, target_platform="linux-x86_64"
                )
                run.assert_called_once_with(
                    "image-security",
                    [
                        sys.executable,
                        "build/security_image.py",
                        options.image,
                        "--artifact-dir",
                        str(output / "export"),
                        "--output",
                        str(output / "image"),
                        "--engine",
                        "docker",
                        "--tools-directory",
                        str(tools),
                    ],
                    timeout=1800,
                )
            with (
                patch.object(
                    security_check, "install", side_effect=ToolError("tool_executable_digest")
                ),
                patch.object(Context, "run") as run,
                self.assertRaisesRegex(ToolError, "tool_executable_digest"),
            ):
                security_check.image(context, options)
            run.assert_not_called()

    def test_missing_or_symlinked_prepared_directory_is_not_reinstalled(self) -> None:
        """An explicit preparation failure cannot turn into an unnoticed serial fallback."""
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            context = Context(ROOT, output)
            link = output / "link"
            link.symlink_to(output, target_is_directory=True)
            for directory in (output / "missing", link):
                with (
                    self.subTest(directory=directory),
                    patch.object(security_check, "install") as install,
                    self.assertRaisesRegex(ToolError, "invalid_prepared_image_tools"),
                ):
                    security_check.image(
                        context,
                        security_check.Options(
                            artifact_dir=output / "export", image_tools=directory
                        ),
                    )
                install.assert_not_called()


if __name__ == "__main__":
    _ = unittest.main()
