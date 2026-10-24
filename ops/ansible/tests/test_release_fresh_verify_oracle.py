"""Whole unchanged local CLI, real framing/extraction and retained effects tests."""

from __future__ import annotations

import hashlib
import http.client
import json
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import release_fresh_verify_oracle as oracle
from test_support import array, obj

# isort: split
import release_attestation as attest
import release_fetch_controller as fetch
import release_fetch_receiver as receiver
import release_trust as trust
from release_json import decode_json

PASSING = {
    "fresh-success",
    "cached-success",
    "fresh-repeat",
    "cached-repeat",
    "run-attempt-large",
    "identifiers-max",
    "api-repository-case",
    "token-spaced-positive",
    "redirect-location-leading-space",
    "redirect-path-percent",
    "redirect-empty-fragment",
    "redirect-100",
    "redirect-body-ignored",
    "blob-length-missing",
    "blob-length-leading-space",
    "blob-100",
    "blob-extra-with-length",
    "required",
    "optional-utc-error",
    "cached-directory-symlink",
    "selector-artifact-plus",
    "selector-ci-underscores",
    "cached-output-parent-relative",
    "cached-directory-relative",
    "artifact-dir-empty",
}


class FreshVerifyOracleTests(unittest.TestCase):
    """External observations never replace actual CLI, trust or filesystem policy."""

    def test_inert_zip_bytes_do_not_follow_the_wall_clock(self) -> None:
        """Parent and child regenerations must keep identical payload, digest and wire bytes."""
        for scenario in ("fresh-success", "zip-crc", "zip-unsafe-name"):
            with self.subTest(scenario=scenario):
                with patch("zipfile.time.localtime", return_value=(2020, 1, 1, 0, 0, 0, 2, 1, -1)):
                    first = oracle.fixture(scenario)
                with patch("zipfile.time.localtime", return_value=(2021, 1, 1, 0, 0, 2, 4, 1, -1)):
                    second = oracle.fixture(scenario)
                self.assertEqual(first.files["artifact.zip"], second.files["artifact.zip"])
                self.assertEqual(first.blob_wire.data, second.blob_wire.data)
                self.assertEqual(first.selection, second.selection)

    def test_all_closed_cases_keep_explicit_verdicts_and_bounded_ordinary_output(self) -> None:
        """Every case runs the real CLI and retains the exact observable failure shape."""
        self.assertEqual(len(oracle.SCENARIOS), len(set(oracle.SCENARIOS)))
        for scenario in oracle.SCENARIOS:
            with self.subTest(scenario=scenario):
                result = oracle.run(scenario)
                observed = obj(result, "observation")
                self.assertEqual(observed["passed"], scenario in PASSING)
                self.assertTrue(observed["cachedOriginalsUnchanged"])
                self.assertLess(
                    len(json.dumps(result, allow_nan=False).encode()), oracle.MAX_OUTPUT_BYTES
                )
                self.assertNotIn(oracle.INERT_TOKEN.decode(), json.dumps(result))
                self.assertNotIn(oracle.GRANT, json.dumps(result))

    def test_original_cli_calls_actual_api_fetch_extract_and_late_verification(self) -> None:
        """Spies delegate complete original bodies instead of returning per-action success."""
        with (
            patch.object(fetch, "verified_envelope", wraps=fetch.verified_envelope) as metadata,
            patch.object(fetch, "download_url", wraps=fetch.download_url) as grant,
            patch.object(attest, "fetch_verify", wraps=attest.fetch_verify) as acquisition,
            patch.object(attest, "verify_directory", wraps=attest.verify_directory) as verify,
            patch.object(receiver, "extract_member", wraps=receiver.extract_member) as extracted,
            patch.object(
                fetch, "Receiver", side_effect=AssertionError("SSH must remain absent")
            ) as ssh,
        ):
            result = oracle.run("fresh-success")
        self.assertTrue(obj(result, "observation")["passed"])
        self.assertEqual(
            (
                metadata.call_count,
                grant.call_count,
                acquisition.call_count,
                verify.call_count,
                extracted.call_count,
                ssh.call_count,
            ),
            (1, 1, 1, 1, 10, 0),
        )

    def test_cached_branch_has_fresh_api_and_crypto_without_token_http_or_curl(self) -> None:
        """An unavailable download tool cannot become a cached-operation prerequisite."""
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary).resolve() / "fixture"
            result = oracle.run("cached-success", directory)
            self.assertFalse((directory / "bin/curl").exists())
            self.assertEqual(stat.S_IMODE((directory / "bin/gh").stat().st_mode), 0o700)
        observed = obj(result, "observation")
        self.assertTrue(observed["passed"])
        self.assertEqual(
            [obj(call)["role"] for call in array(observed, "apiCalls")], ["api-artifact", "api-run"]
        )
        self.assertEqual(array(observed, "httpCalls"), [])
        self.assertEqual(len(array(observed, "verifierCalls")), len(trust.SUBJECTS))
        self.assertFalse(obj(observed, "outputTree")["outputParentExists"])

    def test_every_use_repeats_two_api_reads_and_four_verifiers_with_exact_four_keys(self) -> None:
        """Neither a cached success receipt nor a previous API response can skip work."""
        for scenario in ("fresh-repeat", "cached-repeat"):
            with self.subTest(scenario=scenario):
                observed = obj(oracle.run(scenario), "observation")
                self.assertEqual(array(observed, "exitCodes"), [0, 0])
                calls = array(observed, "apiCalls")
                self.assertEqual(sum(obj(call)["role"] != "auth-token" for call in calls), 4)
                self.assertEqual(len(array(observed, "verifierCalls")), 8)
                self.assertEqual(len(array(observed, "fileSnapshots")), 2)
                for result in array(observed, "results"):
                    self.assertEqual(
                        set(obj(result)), {"revision", "directory", "image.tar", "release.json"}
                    )
                    self.assertEqual(obj(result)["directory"], "$CANDIDATE")
                self.assertEqual(
                    obj(observed, "outputTree")["transactionDirectories"],
                    2 if scenario == "fresh-repeat" else 0,
                )

    def test_token_stays_in_api_memory_and_redirect_body_is_never_read(self) -> None:
        """A real parsed redirect grants no credential/header access to the blob request."""
        observed = obj(oracle.run("fresh-success"), "observation")
        api, blob = (obj(value) for value in array(observed, "httpCalls"))
        self.assertTrue(api["authorizationPresent"])
        self.assertTrue(api["authorizationMatchesInert"])
        self.assertEqual(api["bodyBytesRead"], 0)
        self.assertEqual(api["bodyReads"], 0)
        self.assertFalse(blob["authorizationPresent"])
        self.assertTrue(blob["explicitIdentityEncodingRequested"])
        for call in (api, blob):
            self.assertTrue(call["pipeClosed"])
            self.assertTrue(call["responseClosed"])
            self.assertEqual(call["connectionCloses"], 1)
        for call in array(observed, "verifierCalls"):
            environment = obj(call, "environment")
            self.assertNotIn("UNRELATED", environment)
            self.assertEqual(environment["GH_TOKEN"], "$REDACTED")

    def test_real_stdlib_header_parsing_preserves_specific_accepted_and_refused_cases(self) -> None:
        """Whitespace, duplicates, informational status and declared body framing stay real."""
        for scenario, passed in (
            ("redirect-location-leading-space", True),
            ("redirect-location-trailing-space", False),
            ("redirect-location-folded", False),
            ("redirect-location-duplicate", False),
            ("redirect-empty-fragment", True),
            ("redirect-multiple-fragment", False),
            ("redirect-100", True),
            ("redirect-103", False),
            ("blob-length-missing", True),
            ("blob-length-leading-space", True),
            ("blob-length-trailing-space", False),
            ("blob-length-folded", False),
            ("blob-length-duplicate", False),
            ("blob-content-encoding-duplicate", False),
            ("blob-100", True),
            ("blob-103", False),
            ("blob-extra-with-length", True),
            ("blob-extra-no-length", False),
        ):
            with self.subTest(scenario=scenario):
                with patch.object(
                    http.client, "HTTPResponse", wraps=http.client.HTTPResponse
                ) as parser:
                    result = oracle.run(scenario)
                self.assertEqual(obj(result, "observation")["passed"], passed)
                self.assertGreaterEqual(parser.call_count, 2)
        observed = obj(oracle.run("blob-extra-with-length"), "observation")
        blob = obj(array(observed, "httpCalls")[1])
        self.assertEqual(blob["bodyBytesRead"], obj(observed, "files", "artifact.zip")["bytes"])

    def test_partial_read_deadline_truncation_and_overflow_retain_exact_original_prefixes(
        self,
    ) -> None:
        """Failure never substitutes a successful artifact or discards its written evidence."""
        for scenario, length in (
            ("blob-read-fault", 65536),
            ("blob-deadline", 65536),
            ("blob-partial-truncated", 65539),
            ("blob-partial-extra", 131072),
        ):
            with self.subTest(scenario=scenario):
                result = oracle.run(scenario)
                observed = obj(result, "observation")
                pin = obj(observed, "files", "artifact.zip")
                original = oracle.fixture(scenario).files["artifact.zip"]
                self.assertFalse(observed["passed"])
                self.assertEqual(pin["bytes"], length)
                self.assertEqual(pin["sha256"], hashlib.sha256(original[:length]).hexdigest())
                self.assertEqual(pin["mode"], 0o600)
                self.assertEqual(array(observed, "verifierCalls"), [])
                self.assertEqual(set(obj(observed, "files")), {"artifact.zip"})

    def test_late_relationships_and_fresh_utc_run_after_four_verifier_observations(self) -> None:
        """Bound source/outcome/full manifest and runtime expiry remain late guards."""
        for scenario in (
            "late-source",
            "late-outcome",
            "late-manifest",
            "late-utc-error",
            "late-expiry",
        ):
            with self.subTest(scenario=scenario):
                observed = obj(oracle.run(scenario), "observation")
                self.assertFalse(observed["passed"])
                self.assertEqual(len(array(observed, "verifierCalls")), 4)
        required = obj(oracle.run("required"), "observation")
        self.assertTrue(required["passed"])
        self.assertEqual(required["utcSamples"], 2)
        optional = obj(oracle.run("optional-utc-error"), "observation")
        self.assertTrue(optional["passed"])
        self.assertEqual(optional["utcSamples"], 0)

    def test_profile_differences_and_ignored_cached_parent_are_explicit(self) -> None:
        """Native path/selector/idle domains are named instead of called parity passes."""
        for scenario in oracle.PROFILE:
            with self.subTest(scenario=scenario):
                result = oracle.run(scenario)
                self.assertIsNotNone(obj(result, "input")["rustProfile"])
                self.assertEqual(
                    obj(result, "input")["nativeExpectedPassed"], scenario == "blob-idle-gap"
                )
                self.assertEqual(obj(result, "observation")["passed"], scenario != "blob-idle-gap")
        cached = obj(oracle.run("cached-output-parent-relative"), "observation")
        self.assertTrue(cached["passed"])
        self.assertFalse(obj(cached, "outputTree")["outputParentExists"])
        empty = oracle.run("artifact-dir-empty")
        self.assertEqual(obj(empty, "input")["branch"], "fresh")
        self.assertTrue(obj(empty, "observation")["passed"])

    def test_export_retains_raw_wire_data_and_closed_native_tools_without_running_them(
        self,
    ) -> None:
        """Pristine inputs and executable adapters remain separate from the reference run."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve() / "fixture"
            result = oracle.run("fresh-success", root)
            for response in array(result, "input", "processResponses") + array(
                result, "input", "httpResponses"
            ):
                value = obj(response)
                data = (root / str(value["path"])).read_bytes()
                self.assertEqual(hashlib.sha256(data).hexdigest(), value["sha256"])
                self.assertEqual(len(data), value["bytes"])
            self.assertEqual((root / "native-events.jsonl").read_bytes(), b"")
            for name in ("gh", "curl"):
                script = root / "bin" / name
                self.assertEqual(stat.S_IMODE(script.stat().st_mode), 0o700)
                _ = compile(script.read_text(), str(script), "exec")
            control = obj(decode_json((root / "adapter.json").read_bytes()))
            arguments = array(control, "curlArguments")
            self.assertNotIn("--speed-time", arguments)
            self.assertNotIn("--connect-timeout", arguments)
            self.assertIn("--path-as-is", arguments)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "absent"
            with self.assertRaisesRegex(ValueError, "unknown closed"):
                _ = oracle.run("not-a-case", path)
            self.assertFalse(path.exists())

    def test_native_adapters_preserve_curl_framing_and_actual_associated_run_endpoint(self) -> None:
        """Only fixed inert scripts run; trailing framing and exit18 remain native observations."""
        for scenario, status in [
            ("fresh-success", 0),
            ("blob-extra-with-length", 0),
            ("blob-truncated", 18),
            ("blob-partial-truncated", 18),
        ]:
            with self.subTest(scenario=scenario), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve() / "fixture"
                _ = oracle.run(scenario, root)
                control = obj(decode_json((root / "adapter.json").read_bytes()))
                config = "".join(
                    f"{key} = {json.dumps(value)}\n"
                    for key, value in [
                        ("url", "https://inert-release.blob.core.windows.net/"),
                        ("request-target", "/path/artifact.zip?sig=INERT_SIGNED_GRANT"),
                        ("user-agent", ""),
                        ("connect-timeout", 15),
                        ("header", "Accept: application/zip"),
                        ("header", "Accept-Encoding: identity"),
                    ]
                ).encode()
                arguments = [
                    sys.executable,
                    "-B",
                    str(root / "bin/curl"),
                    *[str(value) for value in array(control, "curlArguments")],
                ]
                completed = subprocess.run(  # noqa: S603 -- Fixed owned inert Python adapter only.
                    arguments,
                    input=config,
                    capture_output=True,
                    check=False,
                    timeout=5,
                    cwd=root,
                    env={"PATH": str(root / "bin"), "LC_ALL": "C"},
                )
                self.assertEqual(completed.returncode, status)
                self.assertEqual(completed.stderr, b"")
                source = oracle.fixture(scenario)
                offset = source.blob_wire.body_offset
                self.assertEqual(completed.stdout[:offset], source.blob_wire.data[:offset])
                self.assertEqual(len(completed.stdout) - offset, control["blobBodyBytes"])
                if scenario == "blob-extra-with-length":
                    self.assertEqual(completed.stdout[offset:], source.files["artifact.zip"])
                else:
                    self.assertEqual(completed.stdout, source.blob_wire.data)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve() / "fixture"
            reference = oracle.run("api-association-run", root)
            control = obj(decode_json((root / "adapter.json").read_bytes()))
            expected = obj(array(obj(reference, "observation"), "apiCalls")[1])
            arguments = [str(value) for value in array(array(control, "metadataArguments")[1])]
            self.assertEqual(["gh", *arguments], expected["arguments"])
            completed = subprocess.run(  # noqa: S603 -- Fixed owned inert Python adapter only.
                [sys.executable, "-B", str(root / "bin/gh"), *arguments],
                stdin=subprocess.DEVNULL,
                capture_output=True,
                check=False,
                timeout=5,
                cwd=root,
                env={"PATH": str(root / "bin"), "LC_ALL": "C"},
            )
            self.assertEqual(completed.returncode, 0)
            self.assertEqual(completed.stderr, b"")
            self.assertEqual(completed.stdout, (root / "responses/process-01.bin").read_bytes())


if __name__ == "__main__":
    _ = unittest.main()
