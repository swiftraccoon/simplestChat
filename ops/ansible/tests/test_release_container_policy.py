"""The fixture's use of the response checker; never contacts Docker or a network endpoint."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Unpack
from unittest.mock import patch

from test_support import ROOT, obj

# isort: split
import release_container_harness as harness
from release_container_harness import CommandOptions
from release_json import decode_json, object_value
from test_release_container_harness import FakeCommands, FakeRun, TextResult

TOKEN = "c" * 32
CADDY = "a" * 64
APP = "b" * 64
RECORDED = ROOT / "crates/ci/src/release_http_policy/recorded_reference.json"


class HarnessResponsePolicyTests(unittest.TestCase):
    """The canonical harness preserves the fixed child and its complete receipt."""

    def test_harness_invokes_fixed_child_and_accepts_only_complete_sanitized_report(self) -> None:
        """The canonical fixture binds its owned identities and keeps raw output private."""
        success = harness.http_policy_success()
        self.assertEqual(success, obj(decode_json(RECORDED.read_bytes()), "matrix", "report"))
        with tempfile.TemporaryDirectory() as temporary:
            current = harness.Harness(
                Path(temporary), "fixture-image", http_policy_binary=Path(sys.executable).resolve()
            )
            current.token = TOKEN
            current.ca_digest = "d" * 64

            calls: list[tuple[list[str], CommandOptions]] = []

            def run(args: list[str], **options: Unpack[CommandOptions]) -> TextResult:
                calls.append((args, options))
                return TextResult(json.dumps(success))

            commands = FakeCommands(run=FakeRun(side_effect=run))
            current.commands = commands
            with patch.object(current, "service", side_effect=[{"id": CADDY}, {"id": APP}]):
                current.response_policy()
            self.assertEqual(object_value(current.report["cases"])["httpResponsePolicy"], success)
            self.assertEqual(len(calls), 1)
            args, options = calls[0]
            self.assertEqual(
                args,
                [
                    str(Path(sys.executable).resolve()),
                    "release-http-policy",
                    "--fixture-token",
                    TOKEN,
                    "--caddy-id",
                    CADDY,
                    "--app-id",
                    APP,
                    "--ca",
                    str(current.private / "ca.crt"),
                    "--ca-sha256",
                    "d" * 64,
                ],
            )
            self.assertEqual(options.get("timeout"), 210)
            self.assertEqual(options.get("grace"), 60)

            for response in (
                {**success, "private": "PRIVATE-FIXTURE"},
                {"passed": True},
                {**success, "passed": 1},
                {**success, "requests": 5.0},
            ):
                with (
                    patch.object(commands, "run", return_value=TextResult(json.dumps(response))),
                    patch.object(current, "service", side_effect=[{"id": CADDY}, {"id": APP}]),
                    self.assertRaisesRegex(harness.CheckError, "did not pass completely"),
                ):
                    current.response_policy()

            failed = TextResult(json.dumps(success), code=42)
            with (
                patch.object(commands, "run", return_value=failed),
                patch.object(current, "service", side_effect=[{"id": CADDY}, {"id": APP}]),
                self.assertRaisesRegex(harness.CheckError, "did not pass completely"),
            ):
                current.response_policy()
