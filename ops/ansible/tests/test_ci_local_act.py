"""Require an intact pinned local runner and atomic publication of completed builds."""

from __future__ import annotations

import hashlib
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "build"))

import ci_local_act as local
from security_tools import ToolError

ROOT = Path(__file__).resolve().parents[3]


class LocalActTests(unittest.TestCase):
    """Keep tool trust checks offline and every fixture within this repository."""

    def directory(self) -> Path:
        """Create one owned disposable fixture directory."""
        temporary = tempfile.TemporaryDirectory(prefix="local-act-test.", dir=ROOT / "results")
        self.addCleanup(temporary.cleanup)
        return Path(temporary.name)

    def test_publish_exposes_binary_and_receipt_together(self) -> None:
        """The final cache is absent until both required artifacts are complete."""
        root = self.directory()
        staging, target = root / "staging", root / "target"
        staging.mkdir()
        _ = (staging / "act").write_bytes(b"completed compiler output")
        local.publish(staging, target, "a" * 64)
        self.assertFalse(staging.exists())
        self.assertEqual(
            json.loads((target / "receipt.json").read_text()),
            {"inputSha256": "a" * 64, "binarySha256": local.digest(target / "act")},
        )

    def test_interrupted_publish_leaves_no_reusable_cache(self) -> None:
        """An interrupted receipt write cannot make a partial build look installed."""
        root = self.directory()
        staging, target = root / "staging", root / "target"
        staging.mkdir()
        _ = (staging / "act").write_bytes(b"completed compiler output")
        with (
            patch.object(Path, "write_text", side_effect=KeyboardInterrupt),
            self.assertRaises(KeyboardInterrupt),
        ):
            local.publish(staging, target, "a" * 64)
        self.assertFalse(target.exists())
        local.publish(staging, target, "a" * 64)
        self.assertTrue((target / "receipt.json").is_file())

    def test_changed_patch_fails_before_compiler_or_network(self) -> None:
        """A maintained patch must match its recorded checksum before tool execution."""
        root = self.directory()
        fixture_patch, pin = root / "act.patch", root / "pin.json"
        _ = fixture_patch.write_text("different patch")
        _ = pin.write_text(json.dumps({"patchSha256": "0" * 64}))
        with (
            patch.object(local, "PATCH", fixture_patch),
            patch.object(local, "PIN", pin),
            patch.object(local, "run") as run,
            self.assertRaisesRegex(ToolError, "local_act_patch_digest"),
        ):
            _ = local.prepare()
        run.assert_not_called()

    def test_cache_reuse_requires_exact_binary_hash(self) -> None:
        """Warm startup accepts complete matching builds and rejects modified bytes."""
        root = self.directory()
        fixture_patch, pin = root / "act.patch", root / "pin.json"
        _ = fixture_patch.write_text("reviewed patch")
        _ = pin.write_text(json.dumps({"patchSha256": local.digest(fixture_patch)}))
        identity = b"go1.27.1\ndarwin\narm64\n"
        key = hashlib.sha256(pin.read_bytes() + identity).hexdigest()
        target = root / ".cache/ci-act" / key
        target.mkdir(parents=True)
        binary = target / "act"
        _ = binary.write_bytes(b"reviewed build")
        binary.chmod(0o700)
        _ = (target / "receipt.json").write_text(
            json.dumps({"inputSha256": key, "binarySha256": local.digest(binary)})
        )
        with (
            patch.object(local, "ROOT", root),
            patch.object(local, "PATCH", fixture_patch),
            patch.object(local, "PIN", pin),
            patch.object(local, "executable", return_value="go"),
            patch.object(local, "run", return_value=identity),
            patch("urllib.request.urlopen") as network,
        ):
            self.assertEqual(local.prepare(), binary)
            _ = binary.write_bytes(b"modified build")
            with self.assertRaisesRegex(ToolError, "local_act_cache_integrity"):
                _ = local.prepare()
        network.assert_not_called()
