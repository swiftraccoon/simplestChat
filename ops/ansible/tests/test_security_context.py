"""Verify scanner transport bounds and credential isolation at the real process boundary."""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "build"))

import security_context


class ProcessTests(unittest.TestCase):
    """Scan reports cannot inherit publishing credentials or exceed their stream budget."""

    def test_environment_omits_credentials_and_python_injection(self) -> None:
        """Explicit tool paths do not require inherited authentication or interpreter hooks."""
        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.dict(
                "os.environ",
                {
                    "GH_TOKEN": "fixture-only",
                    "GITHUB_TOKEN": "fixture-only",
                    "PYTHONPATH": "/untrusted",
                    "PIP_INDEX_URL": "https://untrusted.example.test/simple",
                    "GITLEAKS_CONFIG": "/untrusted",
                },
            ),
        ):
            env = security_context.environment(Path(temporary))
            for name in (
                "GH_TOKEN",
                "GITHUB_TOKEN",
                "PYTHONPATH",
                "PIP_INDEX_URL",
                "GITLEAKS_CONFIG",
            ):
                self.assertNotIn(name, env)

    def test_output_overflow_terminates_before_disk_budget_is_exceeded(self) -> None:
        """The actual owned subprocess is stopped while output is being consumed."""
        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.object(security_context, "MAX_REPORT", 64),
        ):
            root = Path(temporary)
            context = security_context.Context(root, root)
            with self.assertRaises(RuntimeError):
                _ = context.run("overflow", [sys.executable, "-c", "print('x' * 1000)"], timeout=10)
            self.assertLessEqual((root / "01-overflow.stdout").stat().st_size, 64)


if __name__ == "__main__":
    _ = unittest.main()
