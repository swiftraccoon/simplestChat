"""Run the manual free-KVM proof's actual acquisition supervisor against inert faults."""

from __future__ import annotations

import io
import os
import stat
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from typing import TYPE_CHECKING, cast
from unittest.mock import patch

from test_support import ROOT, objects, string, yaml_value

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping, Sequence
    from typing import BinaryIO

sys.path.insert(0, str(ROOT / "build"))
import bounded_process

WORKFLOW = ROOT / ".github/workflows/release-fetch-runtime.yml"


def acquisition_supervisor() -> str:
    """Read the actual fixed inline program rather than copy its policy into a fixture."""
    value = yaml_value(WORKFLOW.read_text(), scalars_as_strings=True)
    steps = objects(value, "jobs", "release-fetch-runtime", "steps")
    runtime = next(step for step in steps if step.get("id") == "runtime")
    lines = string(runtime, "run").splitlines()
    begin, end = lines.index("python3 -B - <<'PY'"), lines.index("PY")
    return "\n".join(lines[begin + 1 : end]) + "\n"


class ReleaseFetchRuntimeWorkflowTests(unittest.TestCase):
    """The inline supervisor keeps raw evidence private and refuses before any VM."""

    def test_actual_acquisition_supervisor_caps_and_closes_before_fixed_diagnostic(self) -> None:
        """Keep all raw evidence private and refuse before VM on timeout/nonzero/IO failure."""
        for fault in ("success", "nonzero", "timeout", "io"):
            with self.subTest(fault=fault), tempfile.TemporaryDirectory() as name:
                root = Path(name).resolve()
                streams: list[BinaryIO] = []
                calls: list[tuple[list[str], float]] = []

                def run(  # noqa: PLR0913 -- Mirror the owned adapter and bind this finite case.
                    arguments: Sequence[str],
                    *,
                    cwd: Path,
                    env: Mapping[str, str],
                    output: BinaryIO,
                    error: BinaryIO,
                    limits: bounded_process.Limits,
                    bound: tuple[str, list[BinaryIO], list[tuple[list[str], float]]] = (
                        fault,
                        streams,
                        calls,
                    ),
                ) -> tuple[int, bytes, bytes]:
                    fault, streams, calls = bound
                    streams.extend((output, error))
                    calls.append((list(arguments), limits.timeout))
                    if cwd != ROOT or env.get("GH_TOKEN") != "INERT_ONLY":
                        raise AssertionError
                    if (limits.stdout, limits.stderr) != (2 * 1024**2, 2 * 1024**2):
                        raise AssertionError
                    _ = output.write(b"INERT_PRIVATE_GRANT\n")
                    _ = error.write(b"INERT_PRIVATE_TOKEN\n")
                    if fault == "io":
                        message = "INERT_PRIVATE_EXCEPTION"
                        raise OSError(message)
                    return (0 if fault == "success" else 124 if fault == "timeout" else 7), b"", b""

                namespace: dict[str, object] = {"__name__": "owned_workflow_fixture"}
                public_output, diagnostic = io.StringIO(), io.StringIO()
                with (
                    patch.dict(
                        os.environ,
                        {
                            "GITHUB_WORKSPACE": str(ROOT),
                            "RUNNER_TEMP": str(root),
                            "GITHUB_REPOSITORY": "swiftraccoon/simplestChat",
                            "GH_TOKEN": "INERT_ONLY",
                        },
                        clear=True,
                    ),
                    patch.object(sys, "path", list(sys.path)),
                    patch.object(bounded_process, "run", run),
                    redirect_stdout(public_output),
                    redirect_stderr(diagnostic),
                ):
                    exec(  # noqa: S102 -- Execute the exact tracked inert workflow program.
                        compile(acquisition_supervisor(), "owned-workflow-supervisor", "exec"),
                        namespace,
                    )
                    status = cast("Callable[[], int]", namespace["main"])()
                closed = all(stream.closed for stream in streams)
                files = list((root / "release-fetch-preacquisition-private").iterdir())
                modes = [stat.S_IMODE(path.stat().st_mode) for path in files]
                retained = b"".join(path.read_bytes() for path in files)
            self.assertEqual(status, 0 if fault == "success" else 1)
            self.assertTrue(closed)
            self.assertEqual(len(calls), 1)
            self.assertEqual(calls[0][1], 1900)
            self.assertEqual(calls[0][0][1:3], ["-B", str(ROOT / "build/security_vm_release.py")])
            self.assertNotIn("security_vm_fetch.py", " ".join(calls[0][0]))
            self.assertEqual(modes, [0o600, 0o600])
            self.assertIn(b"INERT_PRIVATE_GRANT", retained)
            self.assertEqual(public_output.getvalue(), "")
            self.assertNotIn("INERT_PRIVATE", diagnostic.getvalue())
            self.assertEqual(
                diagnostic.getvalue(),
                ""
                if fault == "success"
                else "Signed release preacquisition is unproven; no VM was started.\n",
            )


if __name__ == "__main__":
    _ = unittest.main()
