"""A scanner verdict is provisional until every release evidence digest is recorded."""

from __future__ import annotations

import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path
from unittest.mock import patch

from test_support import ROOT

# isort: split
import release_build
import security_image as image
import security_image_policy as image_policy
from release_json import decode_json, object_value

_ = ROOT
IMAGE = "sha256:" + "a" * 64


def passing_scans(stack: ExitStack, output: Path) -> None:
    """Replace scanner boundaries while exercising the real final outcome and file writes."""
    fixed: dict[str, object] = {
        "bind_archive": {"revision": "b" * 40, "archiveSha256": "c" * 64},
        "native_binding": {"binary": {"sha256": "d" * 64}},
        "prepare_sandbox": None,
        "selected_image": {"id": IMAGE},
        "secret_selftest": None,
        "scan_reports": (output / "sbom", output / "grype", output / "secrets", {}),
    }
    for name, value in fixed.items():
        _ = stack.enter_context(patch.object(image, name, return_value=value))
    policy: dict[str, object] = {
        "load_policy": {},
        "report": {},
        "database_status": None,
        "inventory": [],
        "vulnerability_database_binding": None,
        "runtime_rpm_bindings": [],
        "vulnerability_verdict": {"passed": True},
        "license_verdict": {"passed": True},
        "secret_verdict": {"passed": True},
    }
    for name, value in policy.items():
        _ = stack.enter_context(patch.object(image_policy, name, return_value=value))
    _ = stack.enter_context(patch.object(release_build.Runner, "run", return_value=""))


class ImageFinalizationTests(unittest.TestCase):
    """A successful set of checks cannot conceal a late evidence read failure."""

    def test_complete_evidence_publishes_pass(self) -> None:
        """The successful path still records all five evidence bindings and check results."""
        with tempfile.TemporaryDirectory() as temporary, ExitStack() as stack:
            output = Path(temporary).resolve() / "out"
            passing_scans(stack, output)
            _ = stack.enter_context(patch.object(image, "digest", return_value="e" * 64))
            self.assertTrue(image.execute(image.Options(image_id=IMAGE, output=output)))
            outcome = object_value(decode_json((output / "outcome.json").read_text()))
            self.assertTrue(outcome["passed"])
            self.assertNotIn("error", outcome)
            for name in ("secretPathMap", "databaseEvidence", "sbom", "native", "elf"):
                self.assertEqual(outcome[name + "Sha256"], "e" * 64)

    def test_final_evidence_failure_keeps_successful_checks_but_fails_outcome(self) -> None:
        """A missing SPDX report after policy evaluation must fail both receipt and exit."""
        errors = (FileNotFoundError, OSError, ValueError, KeyError)
        for error_type in errors:
            with (
                self.subTest(error=error_type.__name__),
                tempfile.TemporaryDirectory() as temporary,
                ExitStack() as stack,
            ):
                output = Path(temporary).resolve() / "out"
                passing_scans(stack, output)

                def final_digest(
                    path: Path,
                    target: Path = output / "spdx/sbom.spdx.json",
                    error_class: type[Exception] = error_type,
                ) -> str:
                    if path == target:
                        reason = "inert final evidence read failure"
                        raise error_class(reason)
                    return "e" * 64

                _ = stack.enter_context(patch.object(image, "digest", side_effect=final_digest))
                self.assertFalse(image.execute(image.Options(image_id=IMAGE, output=output)))
                outcome = object_value(decode_json((output / "outcome.json").read_text()))
                self.assertFalse(outcome["passed"])
                self.assertEqual(outcome["error"], error_type.__name__)
                self.assertNotIn("sbomSha256", outcome)
                checks = object_value(decode_json((output / "checks.json").read_text()))
                self.assertEqual(
                    checks,
                    {name: {"passed": True} for name in ("vulnerabilities", "licenses", "secrets")},
                )
