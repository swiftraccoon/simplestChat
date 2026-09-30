"""Require signed-release verification before a VM acquisition is usable."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import test_support

# isort: split
import release_attestation as attestation
import release_deploy as deploy
import release_fetch_controller as fetch
import security_vm_release as acquisition

REVISION = "a" * 40
REPOSITORY = "swiftraccoon/simplestChat"


class VmReleaseTests(unittest.TestCase):
    """Mock only remote boundaries; tests create no engine, VM or HTTP request."""

    def test_success_uses_one_exact_selection_and_signature_verifier(self) -> None:
        """A completion record appears only after cryptographic verification returns."""
        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.object(deploy, "checkout_identity", return_value=REVISION),
            patch.object(
                deploy,
                "select_ci_artifact",
                return_value={"artifactId": 3, "ciRunId": 4},
            ) as select,
            patch.object(
                fetch, "download_url", return_value="https://example.invalid/inert"
            ) as url,
            patch.object(attestation, "fetch_verify") as verify,
        ):
            output = Path(temporary) / "release"
            acquisition.prepare(REPOSITORY, output)
            select.assert_called_once()
            url.assert_called_once_with(
                fetch.ReleaseSelection(
                    repository=REPOSITORY, revision=REVISION, artifact_id=3, ci_run=4
                )
            )
            verify.assert_called_once_with(
                "https://example.invalid/inert",
                output / "artifact",
                {"artifactId": 3, "ciRunId": 4},
            )
            record = test_support.obj(
                test_support.yaml_value((output / "selection.json").read_text())
            )
            self.assertEqual(record, {"revision": REVISION, "ciRunId": 4, "artifactId": 3})

    def test_failed_signature_leaves_no_completion_record(self) -> None:
        """Downloaded bytes alone cannot authorize the next guest step."""
        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.object(deploy, "checkout_identity", return_value=REVISION),
            patch.object(
                deploy,
                "select_ci_artifact",
                return_value={"artifactId": 3, "ciRunId": 4},
            ),
            patch.object(fetch, "download_url", return_value="https://example.invalid/inert"),
            patch.object(attestation, "fetch_verify", side_effect=ValueError("invalid signature")),
        ):
            output = Path(temporary) / "release"
            with self.assertRaises(ValueError):
                acquisition.prepare(REPOSITORY, output)
            self.assertFalse((output / "selection.json").exists())

    def test_source_identity_failure_precedes_artifact_access(self) -> None:
        """An uncommitted or wrong-origin checkout must not select a release."""
        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.object(
                deploy,
                "checkout_identity",
                side_effect=deploy.DeployError("dirty"),
            ),
            patch.object(deploy, "select_ci_artifact") as select,
        ):
            with self.assertRaises(deploy.DeployError):
                acquisition.prepare(REPOSITORY, Path(temporary) / "release")
            select.assert_not_called()


if __name__ == "__main__":
    _ = unittest.main()
