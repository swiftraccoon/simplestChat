"""Whole cached-verification fixtures preserve real policy and passive effects."""

from __future__ import annotations

import hashlib
import json
import tempfile
import unittest
import warnings
from pathlib import Path
from unittest.mock import patch

import release_verify_oracle as oracle
from test_support import array, obj

# isort: split
import release_attestation as attest
import release_fetch_receiver as receiver
import release_trust as trust
from release_json import JsonObject, decode_json


class ReleaseVerifyOracleTests(unittest.TestCase):
    """Exercise real verifier orchestration, ZIP/trust relationships and late guards."""

    def test_reference_whole_flow_and_fresh_repeat_are_not_replaced(self) -> None:
        """Every use repeats cached ZIP verification and four actual verifier dispatches."""
        with (
            patch.object(attest, "verify_cached_zip", wraps=attest.verify_cached_zip) as cached,
            patch.object(trust, "bind_files", wraps=trust.bind_files) as binding,
            patch.object(
                receiver, "validate_build_evidence", wraps=receiver.validate_build_evidence
            ) as final,
        ):
            result = oracle.run("repeat-fresh")
        self.assertEqual(cached.call_count, 2)
        self.assertEqual(binding.call_count, 4)
        self.assertEqual(final.call_count, 2)
        self.assertEqual(array(result, "observation", "passes"), [True, True])
        self.assertEqual(len(array(result, "observation", "calls")), 8)
        self.assertTrue(obj(result, "observation")["candidateUnchanged"])
        self.assertNotIn("attestation", obj(result, "input", "selection"))

    def test_exact_verifier_arguments_environment_caps_and_raw_hashes(self) -> None:
        """All four calls retain fixed identity restrictions and exact returned raw bytes."""
        result = oracle.run("success")
        calls = array(result, "observation", "calls")
        proof = obj(array(result, "observation", "results")[0], "attestation")
        expected_flags = [
            "--bundle",
            "$CANDIDATE/release-attestation.jsonl",
            "--repo",
            "owner/repo",
            "--hostname",
            "github.com",
            "--predicate-type",
            trust.PREDICATE_TYPE,
            "--cert-identity",
            "https://github.com/owner/repo/.github/workflows/ci.yml@refs/heads/main",
            "--cert-oidc-issuer",
            trust.ISSUER,
            "--signer-digest",
            "a" * 40,
            "--source-digest",
            "a" * 40,
            "--source-ref",
            "refs/heads/main",
            "--deny-self-hosted-runners",
            "--format",
            "json",
        ]
        for name, value in zip(trust.SUBJECTS, calls, strict=True):
            call = obj(value)
            self.assertEqual(call["subject"], name)
            self.assertEqual(
                call["arguments"],
                ["gh", "attestation", "verify", f"$CANDIDATE/{name}", *expected_flags],
            )
            self.assertEqual(call["environment"], oracle.ENVIRONMENT)
            self.assertEqual(
                (call["timeoutSeconds"], call["stdoutLimit"], call["stderrLimit"]),
                (180, 8 * 1024**2, 65536),
            )
            self.assertEqual(obj(proof, "verificationSha256")[name], call["responseSha256"])

    def test_certificate_statement_and_semantic_response_cases(self) -> None:
        """No certificate restriction or exact subject membership can be discarded."""
        negative = [
            *(f"gh-certificate-{key}" for key in oracle.CERTIFICATE_KEYS),
            "gh-no-timestamp",
            "gh-statement-type",
            "gh-predicate-type",
            "gh-claim",
            "gh-subject-missing",
            "gh-subject-duplicate",
            "gh-subject-extra",
            "gh-invalid-json",
            "gh-duplicate-json",
            "gh-empty",
            "gh-multiple",
        ]
        for scenario in negative:
            with self.subTest(scenario=scenario):
                result = oracle.run(scenario)
                self.assertFalse(obj(result, "observation")["passed"])
                self.assertEqual(len(array(result, "observation", "calls")), 1)
        for scenario in (
            "gh-subject-reordered",
            "gh-claim-schema-bool",
            "gh-claim-run-float",
            "gh-large-integer",
            "gh-utf16",
            "gh-surrogate",
        ):
            with self.subTest(scenario=scenario):
                self.assertTrue(obj(oracle.run(scenario), "observation")["passed"])

    def test_zip_accepted_domain_includes_unselected_metadata_quirks(self) -> None:
        """Cached comparison does not impose a full extraction profile on unselected files."""
        accepted = (
            "zip-store",
            "zip-store-trailing",
            "zip-deflate-extra-output",
            "zip-comment",
            "zip-local-zip64",
            "zip-central-zip64",
            "zip-extra-opaque",
            "zip-extra-bad-crc",
            "zip-directory-data",
            "zip-prefix-files",
            "zip-large-central-record",
            "zip-local-metadata-diff",
            "zip-local-flags-diff",
            "zip-unselected-flags",
            "zip-shared-offset-extra-first",
            "zip-shared-offset-extra-last",
            "zip-shared-offset-wide-extent",
            "zip-eocd-count-diff",
        )
        for scenario in accepted:
            with self.subTest(scenario=scenario):
                result = oracle.run(scenario)
                self.assertTrue(obj(result, "observation")["passed"])
                self.assertEqual(len(array(result, "observation", "calls")), 4)
        for scenario in (
            name for name in oracle.SCENARIOS if name.startswith("zip-") and name not in accepted
        ):
            with self.subTest(scenario=scenario):
                result = oracle.run(scenario)
                self.assertFalse(obj(result, "observation")["passed"])
                self.assertEqual(array(result, "observation", "calls"), [])

    def test_selected_central_flags_control_opening_but_local_and_unselected_bits_do_not(
        self,
    ) -> None:
        """Keep patched/strong checks at the exact selected central-header boundary."""
        for scenario, passed, calls in (
            ("zip-local-flags-diff", True, 4),
            ("zip-unselected-flags", True, 4),
            ("zip-selected-patched-flag", False, 0),
            ("zip-selected-strong-flag", False, 0),
            ("zip-encrypted-extra", False, 0),
        ):
            with self.subTest(scenario=scenario):
                result = oracle.run(scenario)
                self.assertEqual(obj(result, "observation")["passed"], passed)
                self.assertEqual(len(array(result, "observation", "calls")), calls)

    def test_same_offset_central_order_and_only_the_declared_warning_are_preserved(self) -> None:
        """Both central orders pass; unrelated reference warnings remain visible failures."""
        for scenario in (
            "zip-shared-offset-extra-first",
            "zip-shared-offset-extra-last",
            "zip-shared-offset-wide-extent",
        ):
            with self.subTest(scenario=scenario):
                result = oracle.run(scenario)
                self.assertTrue(obj(result, "observation")["passed"])
                self.assertEqual(len(array(result, "observation", "calls")), 4)

        def unexpected(_candidate: Path, _selection: JsonObject) -> JsonObject:
            warnings.warn("unrelated diagnostic", UserWarning, stacklevel=2)
            return {}

        with (
            patch.object(attest, "verify_directory", side_effect=unexpected),
            self.assertRaisesRegex(RuntimeError, "unexpected warning"),
        ):
            _ = oracle.verify_reference(Path("unused"), {}, "success")

    def test_late_relationships_still_run_after_successful_crypto_observations(self) -> None:
        """Source/outcome/manifest/archive guards cannot be replaced with passed flags."""
        for scenario in (
            "late-source",
            "late-outcome-error",
            "late-manifest-date",
            "late-archive",
            "mutate-bundle",
            "mutate-source",
            "mutate-outcome",
            "mutate-manifest",
        ):
            with self.subTest(scenario=scenario):
                result = oracle.run(scenario)
                self.assertFalse(obj(result, "observation")["passed"])
                self.assertEqual(len(array(result, "observation", "calls")), 4)
        for scenario in ("mutate-zip", "exchange-bundle"):
            with self.subTest(scenario=scenario):
                result = oracle.run(scenario)
                self.assertTrue(obj(result, "observation")["passed"])
                self.assertFalse(obj(result, "observation")["candidateUnchanged"])
                self.assertIsNotNone(obj(result, "input")["rustProfile"])

    def test_runtime_utc_is_lazy_and_revalidated_at_final_binding(self) -> None:
        """Optional disposition never samples UTC; required final binding samples afresh."""
        for scenario, passed, calls, samples in (
            ("optional-utc-error", True, 4, 0),
            ("required", True, 4, 2),
            ("required-utc-error", False, 0, 1),
            ("late-utc-error", False, 4, 2),
            ("late-expiry", False, 4, 2),
            ("expiry-today", False, 0, 1),
            ("expiry-yesterday", False, 0, 1),
            ("expiry-unicode", False, 0, 0),
        ):
            with self.subTest(scenario=scenario):
                result = oracle.run(scenario)
                self.assertEqual(obj(result, "observation")["passed"], passed)
                self.assertEqual(len(array(result, "observation", "calls")), calls)
                self.assertEqual(obj(result, "observation")["utcSamples"], samples)

    def test_export_preserves_original_inputs_and_pinned_external_raw_responses(self) -> None:
        """The reference mutates its separate candidate, never the native consumer's input."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve() / "input"
            result = oracle.run("mutate-bundle", root)
            original = oracle.fixture("mutate-bundle")
            for name, data in original.files.items():
                self.assertEqual((root / "candidate" / name).read_bytes(), data)
            for response in array(result, "input", "responses"):
                pin = obj(response)
                path = root / str(pin["path"])
                self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), pin["sha256"])
                self.assertEqual(path.stat().st_size, pin["bytes"])
            self.assertEqual(
                set(obj(decode_json((root / "metadata.json").read_bytes()))),
                {
                    "schemaVersion",
                    "repository",
                    "artifactId",
                    "buildRunId",
                    "ciRunId",
                    "runAttempt",
                    "revision",
                    "artifactZipBytes",
                    "zipSha256",
                },
            )
            self.assertFalse(obj(result, "observation")["passed"])
            self.assertLess(len(json.dumps(result)), oracle.MAX_OUTPUT_BYTES)

    def test_every_closed_fixture_runs_with_bounded_ordinary_output(self) -> None:
        """All declared cases execute the real whole operation without process or network use."""
        self.assertEqual(len(oracle.SCENARIOS), len(set(oracle.SCENARIOS)))
        for scenario in oracle.SCENARIOS:
            with self.subTest(scenario=scenario):
                result = oracle.run(scenario)
                self.assertEqual(result["scenario"], scenario)
                self.assertLess(len(json.dumps(result, allow_nan=False)), oracle.MAX_OUTPUT_BYTES)
                self.assertLessEqual(len(array(result, "observation", "calls")), 8)
                self.assertEqual(len(obj(result, "observation", "files")), 11)
        with tempfile.TemporaryDirectory() as temporary:
            target = Path(temporary) / "must-not-exist"
            with self.assertRaisesRegex(ValueError, "unknown closed"):
                _ = oracle.run("unknown", target)
            self.assertFalse(target.exists())


if __name__ == "__main__":
    _ = unittest.main()
