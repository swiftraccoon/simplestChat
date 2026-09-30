"""Real local process fixtures for output budgets, timeouts and lease cancellation."""

from __future__ import annotations

import io
import os
import signal
import subprocess
import sys
import time
import unittest
from unittest.mock import Mock, patch

from test_support import ROOT

# isort: split

import bounded_process as process


class BoundedProcessTests(unittest.TestCase):
    """A child must not spend disk or memory beyond its declared output budget."""

    def test_input_and_both_output_streams_are_serviced_without_deadlock(self) -> None:
        """Large stdin and noisy stderr are concurrent, independently bounded streams."""
        script = (
            "import sys; sys.stderr.buffer.write(b'e'*100000); sys.stderr.flush(); "
            + "sys.stdout.buffer.write(sys.stdin.buffer.read())"
        )
        status, output, error = process.run(
            [sys.executable, "-c", script], cwd=ROOT, input_data=b"x" * 200000
        )
        self.assertEqual((status, output, error), (0, b"x" * 200000, b"e" * 100000))

    def test_output_limit_applies_before_writing_the_overflow_chunk(self) -> None:
        """Either destination stops exactly at its budget and the child is reaped."""
        for channel in ("stdout", "stderr"):
            with self.subTest(channel=channel):
                output, error = io.BytesIO(), io.BytesIO()
                with self.assertRaisesRegex(process.ProcessError, channel + "_limit_exceeded"):
                    _ = process.run(
                        [
                            sys.executable,
                            "-c",
                            f"import sys,time; sys.{channel}.buffer.write(b'x'*100000); "
                            + f"sys.{channel}.flush(); time.sleep(30)",
                        ],
                        output=output,
                        error=error,
                        limits=process.Limits(timeout=2, stdout=1024, stderr=2048),
                    )
                self.assertLessEqual(len(output.getvalue()), 1024)
                self.assertLessEqual(len(error.getvalue()), 2048)

    def test_quiet_child_has_a_deadline_and_health_check(self) -> None:
        """No output is needed to notice a deadline or a lost remote lease."""
        for limits, code in (
            (process.Limits(timeout=0.1), "command_timed_out"),
            (process.Limits(timeout=10, healthy=lambda: False), "command_lease_lost"),
        ):
            started = time.monotonic()
            with self.subTest(code=code), self.assertRaisesRegex(process.ProcessError, code):
                _ = process.run(
                    [sys.executable, "-c", "import time; time.sleep(30)"], limits=limits
                )
            self.assertLess(time.monotonic() - started, 3)

    def test_nonzero_status_and_partial_output_remain_observable(self) -> None:
        """The transport returns command failures for its caller's explicit policy."""
        self.assertEqual(
            process.run([sys.executable, "-c", "print('retained'); raise SystemExit(7)"]),
            (7, b"retained\n", b""),
        )

    def test_health_is_checked_after_both_output_descriptors_close(self) -> None:
        """A quiet command cannot hide from cancellation by closing its output early."""
        started = time.monotonic()
        cancel_after = 0.3
        with self.assertRaisesRegex(process.ProcessError, "command_lease_lost"):
            _ = process.run(
                [sys.executable, "-c", "import os,time; os.close(1); os.close(2); time.sleep(30)"],
                limits=process.Limits(
                    timeout=10, healthy=lambda: time.monotonic() - started < cancel_after
                ),
            )
        self.assertLess(time.monotonic() - started, 2)

    def test_descendant_is_stopped_even_when_its_leader_accepts_term(self) -> None:
        """The group remains owned until its TERM-ignoring child is killed and leader reaped."""
        script = """import os,signal,time
pid = os.fork()
if pid == 0:
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    print(os.getpid(), flush=True)
    time.sleep(5)
else:
    time.sleep(30)
"""
        captured = io.BytesIO()
        with self.assertRaisesRegex(process.ProcessError, "command_timed_out"):
            _ = process.run(
                [sys.executable, "-c", script], output=captured, limits=process.Limits(timeout=0.3)
            )
        descendant = int(captured.getvalue().strip())
        _, state, _ = process.run(["/bin/ps", "-p", str(descendant), "-o", "stat="])
        stopped = not state.strip() or state.strip().startswith(b"Z")
        if not stopped:
            os.kill(descendant, signal.SIGKILL)
        self.assertTrue(stopped, "an owned descendant survived group cleanup")

    def test_darwin_permission_error_requires_complete_zombie_only_group_evidence(self) -> None:
        """Live, foreign, missing, duplicated and malformed rows cannot excuse EPERM."""
        for body, accepted in (
            (b" 123 123 Zs\n", True),
            (b"123 123 Zs\n124 123 Z\n", True),
            (b"123 123 Zs\n124 123 S\n", False),
            (b"123 123 Zs\n124 123 ?\n", False),
            (b"123 123 Zombie\n", False),
            (b"123 123 Zs\n" + b"9" * 5000 + b" 123 Z\n", False),
            (b"123 123 Zs\n124 999 Z\n", False),
            (b"124 123 Z\n", False),
            (b"123 123 Zs\n123 123 Zs\n", False),
            (b"123 123 Zs\nunknown\n", False),
            (b"", False),
        ):
            output, error = io.BytesIO(body), io.BytesIO()
            with (
                self.subTest(body=body),
                patch.object(sys, "platform", "darwin"),
                patch.object(os, "killpg", side_effect=PermissionError),
                patch.object(process, "leader_exited", return_value=True),
                patch.object(io, "BytesIO", side_effect=[output, error]),
                patch.object(
                    subprocess, "Popen", return_value=Mock(stdout=None, stderr=None)
                ) as launch,
                patch.object(process, "pump", return_value=0) as pump,
            ):
                if accepted:
                    process.signal_group(123, signal.SIGKILL)
                else:
                    with self.assertRaises(PermissionError):
                        process.signal_group(123, signal.SIGKILL)
                self.assertEqual(
                    launch.call_args.args[0],
                    ["/bin/ps", "-g", "123", "-o", "pid=,pgid=,stat="],
                )
                self.assertEqual(
                    pump.call_args.args[3], process.Limits(timeout=2, stdout=65536, stderr=65536)
                )

    def test_permission_errors_remain_failures_on_linux_and_for_live_leaders(self) -> None:
        """No general EPERM suppression or leader-reaping liveness probe is allowed."""
        for platform, exited in (("linux", True), ("darwin", False)):
            with (
                self.subTest(platform=platform),
                patch.object(sys, "platform", platform),
                patch.object(os, "killpg", side_effect=PermissionError),
                patch.object(process, "leader_exited", return_value=exited),
                patch.object(subprocess, "Popen") as launch,
                self.assertRaises(PermissionError),
            ):
                process.signal_group(123, signal.SIGTERM)
            launch.assert_not_called()

    def test_zombie_query_failure_kills_only_its_direct_utility_and_fails_closed(self) -> None:
        """A timed-out or oversized inspection never recurses through group cleanup."""
        for failure in (
            process.ProcessError("command_stdout_limit_exceeded"),
            subprocess.TimeoutExpired("ps", 2),
            OSError(),
        ):
            kill, wait = Mock(), Mock(return_value=0)
            child = Mock(stdout=None, stderr=None, kill=kill, wait=wait)
            with (
                self.subTest(failure=type(failure).__name__),
                patch.object(process, "leader_exited", return_value=True),
                patch.object(subprocess, "Popen", return_value=child),
                patch.object(process, "pump", side_effect=failure),
                patch.object(process, "stop") as stop,
            ):
                self.assertFalse(process.darwin_zombie_group(123))
                kill.assert_called_once_with()
                wait.assert_called_once_with(timeout=5)
                stop.assert_not_called()
        with (
            patch.object(process, "leader_exited", return_value=True),
            patch.object(subprocess, "Popen", side_effect=OSError),
        ):
            self.assertFalse(process.darwin_zombie_group(123))

    def test_zombie_query_requires_successful_silent_complete_result(self) -> None:
        """Nonzero exit, diagnostic output or lost ownership is never treated as proof."""
        for status, diagnostic, still_owned in (
            (1, b"", True),
            (0, b"warning", True),
            (0, b"", False),
        ):
            output, error = io.BytesIO(b"123 123 Zs\n"), io.BytesIO(diagnostic)
            with (
                self.subTest(status=status, diagnostic=diagnostic, still_owned=still_owned),
                patch.object(process, "leader_exited", side_effect=[True, still_owned]),
                patch.object(io, "BytesIO", side_effect=[output, error]),
                patch.object(subprocess, "Popen"),
                patch.object(process, "pump", return_value=status),
            ):
                self.assertFalse(process.darwin_zombie_group(123))

    @unittest.skipUnless(sys.platform == "darwin", "Darwin process-group zombie behavior")
    def test_darwin_real_unreaped_zombie_is_identified_before_reaping(self) -> None:
        """The real system utility observes the reserved leader without consuming its status."""
        with subprocess.Popen(
            [sys.executable, "-c", "pass"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        ) as child:
            self.assertTrue(process.wait_exited(child.pid, 5))
            self.assertTrue(process.darwin_zombie_group(child.pid))
            self.assertIsNone(child.returncode)
            process.signal_group(child.pid, signal.SIGKILL)
            self.assertEqual(child.wait(timeout=2), 0)


if __name__ == "__main__":
    _ = unittest.main()
