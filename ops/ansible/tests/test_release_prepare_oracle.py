"""Exercise complete unchanged preparation, its retained effects and fixture exports."""

from __future__ import annotations

import hashlib
import io
import json
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

from test_support import array, obj, string

# isort: split
import release_prepare_oracle as oracle
import release_trust as trust
from release_json import decode_json

PRIVATE_FILE_MODE = 0o600
PASSING = {
    "success",
    "required",
    "opaque-source",
    "opaque-sbom",
    "hash-only-empty",
    "hash-only-raw",
    "hash-only-symlink",
    "encoding-security-bom",
    "encoding-security-utf16",
    "encoding-runtime-utf32",
    "ignored-large-integer",
    "ignored-bool-null",
    "ignored-surrogate",
    "ignored-depth",
    "ignored-values",
}


class ReleasePrepareOracleTests(unittest.TestCase):
    """Real files/archive/copy/trust determine every result; no service is contacted."""

    def test_all_closed_cases_run_the_actual_whole_operation_and_preserve_originals(
        self,
    ) -> None:
        """Every finite case has an explicit verdict and ordinary bounded output."""
        self.assertEqual(len(oracle.SCENARIOS), len(set(oracle.SCENARIOS)))
        for scenario in oracle.SCENARIOS:
            with self.subTest(scenario=scenario):
                result = oracle.run_scenario(scenario)
                observed = obj(result, "observation")
                self.assertEqual(result["schemaVersion"], 1)
                self.assertEqual(result["scenario"], scenario)
                self.assertEqual(observed["passed"], scenario in PASSING)
                self.assertTrue(observed["originalUnchanged"] is True)
                self.assertNotIn(trust.BUNDLE, obj(observed, "files"))
                self.assertLess(
                    len(json.dumps(result, allow_nan=False).encode()),
                    oracle.MAX_OUTPUT_BYTES,
                )
                if scenario in PASSING:
                    self.assertEqual(
                        set(obj(observed, "files")),
                        {*trust.BOUND_FILES, trust.PREDICATE},
                    )
                    self.assertEqual(array(observed, "copyOrder"), list(oracle.COPIES))
                    self.assertEqual(
                        set(obj(observed, "predicate", "fileDigests")),
                        set(trust.BOUND_FILES),
                    )
                    self.assertEqual(
                        observed["predicateSha256"],
                        obj(observed, "files", trust.PREDICATE)["sha256"],
                    )
                else:
                    self.assertIsNone(observed["predicate"])
                    self.assertIsNone(observed["predicateSha256"])

    def test_success_claim_and_exact_output_bytes_are_deterministic(self) -> None:
        """Fixture path, wall clock and run order do not change the reference predicate."""
        first = oracle.run_scenario("success")
        self.assertEqual(first, oracle.run_scenario("success"))
        observed = obj(first, "observation")
        claim = obj(observed, "predicate")
        self.assertEqual(claim["runId"], 789)
        self.assertEqual(claim["runAttempt"], 1)
        self.assertTrue(claim["securityPassed"] is True)
        expected = (json.dumps(claim, indent=2, sort_keys=True) + "\n").encode()
        self.assertEqual(hashlib.sha256(expected).hexdigest(), observed["predicateSha256"])
        self.assertTrue(
            all(
                obj(value)["mode"] == PRIVATE_FILE_MODE for value in obj(observed, "files").values()
            )
        )

    def test_each_evidence_hash_refusal_precedes_all_copies(self) -> None:
        """Each of the eight original scanner evidence relationships is checked."""
        for _, _, label in oracle.EVIDENCE:
            with self.subTest(label=label):
                observed = obj(oracle.run_scenario("hash-" + label), "observation")
                self.assertTrue(observed["passed"] is False)
                self.assertEqual(array(observed, "copyOrder"), [])
                self.assertEqual(set(obj(observed, "files")), set(oracle.ORIGINAL))

    def test_conflicts_retain_existing_bytes_and_only_completed_copy_prefix(
        self,
    ) -> None:
        """Create-new refusal leaves previous files, including the conflicting file, intact."""
        for index, label in enumerate(oracle.COPY_LABELS):
            with self.subTest(label=label):
                observed = obj(oracle.run_scenario("conflict-" + label), "observation")
                files = obj(observed, "files")
                self.assertEqual(array(observed, "copyOrder"), list(oracle.COPIES[:index]))
                self.assertEqual(set(files), {*oracle.ORIGINAL, *oracle.COPIES[: index + 1]})
                self.assertEqual(
                    obj(files, oracle.COPIES[index])["sha256"],
                    oracle.digest(b"existing destination remains unchanged\n"),
                )
        observed = obj(oracle.run_scenario("conflict-predicate"), "observation")
        self.assertEqual(array(observed, "copyOrder"), list(oracle.COPIES))
        self.assertEqual(
            obj(observed, "files", trust.PREDICATE)["sha256"],
            oracle.digest(b"existing predicate remains unchanged\n"),
        )

    def test_read_and_write_faults_retain_actual_three_byte_current_file(self) -> None:
        """The unmodified copy loop runs with one narrowly adapted real reader/writer."""
        for kind in ("read", "write"):
            for index, label in enumerate(oracle.COPY_LABELS):
                with self.subTest(kind=kind, label=label):
                    result = oracle.run_scenario(f"copy-{kind}-{label}")
                    observed = obj(result, "observation")
                    self.assertEqual(array(observed, "copyOrder"), list(oracle.COPIES[: index + 1]))
                    self.assertEqual(
                        set(obj(observed, "files")),
                        {*oracle.ORIGINAL, *oracle.COPIES[: index + 1]},
                    )
                    self.assertEqual(obj(observed, "files", oracle.COPIES[index])["bytes"], 3)
                    self.assertEqual(
                        obj(result, "input", "fault"),
                        {"kind": "copy-" + kind, "copy": index, "retainedBytes": 3},
                    )

    def test_exports_are_pristine_bounded_inputs_before_reference_effects(self) -> None:
        """A candidate can consume the exact original four files without copy conflicts."""
        for scenario in (
            "success",
            "required",
            "hash-only-symlink",
            "copy-read-runtime-proof",
        ):
            with (
                self.subTest(scenario=scenario),
                tempfile.TemporaryDirectory() as temporary,
            ):
                output = Path(temporary).resolve() / "fixture"
                result = oracle.run_scenario(scenario, output)
                self.assertEqual(
                    {path.name for path in (output / "artifact").iterdir()},
                    set(oracle.ORIGINAL),
                )
                self.assertFalse((output / "artifact" / trust.PREDICATE).exists())
                observed = obj(result, "observation", "files")
                for name in oracle.ORIGINAL:
                    self.assertEqual(
                        oracle.digest((output / "artifact" / name).read_bytes()),
                        obj(observed, name)["sha256"],
                    )
                self.assertLess(
                    sum(path.stat().st_size for path in output.rglob("*") if path.is_file()),
                    oracle.MAX_FIXTURE_BYTES,
                )
                if scenario == "hash-only-symlink":
                    self.assertTrue((output / "security/native.json").is_symlink())
                    self.assertEqual(obj(result, "input")["rustProfile"], "nofollow-hash-only")

    def test_opaque_inputs_remain_opaque_and_profile_differences_are_explicit(
        self,
    ) -> None:
        """Preparation does not invent source/SPDX or hash-only evidence schemas."""
        for scenario in (
            "opaque-source",
            "opaque-sbom",
            "hash-only-empty",
            "hash-only-raw",
        ):
            self.assertTrue(obj(oracle.run_scenario(scenario), "observation")["passed"] is True)
        for scenario, profile in oracle.PROFILE.items():
            result = oracle.run_scenario(scenario)
            self.assertEqual(obj(result, "input")["rustProfile"], profile)
            self.assertTrue(obj(result, "observation")["passed"] is True)

    def test_fixed_utc_expiry_is_exclusive_and_uses_real_calendar_validation(
        self,
    ) -> None:
        """Tomorrow passes; today, yesterday, invalid dates and non-ASCII spelling fail."""
        self.assertTrue(obj(oracle.run_scenario("required"), "observation")["passed"] is True)
        for scenario in (
            "expiry-today",
            "expiry-yesterday",
            "expiry-invalid",
            "expiry-unicode",
        ):
            result = oracle.run_scenario(scenario)
            self.assertEqual(obj(result, "input")["today"], "2050-01-01")
            self.assertTrue(obj(result, "observation")["passed"] is False)
            self.assertEqual(array(result, "observation", "copyOrder"), list(oracle.COPIES))

    def test_cli_is_closed_and_returns_only_bounded_ordinary_json(self) -> None:
        """The standalone entrypoint needs no credentials or source execution adapters."""
        output = io.StringIO()
        with redirect_stdout(output):
            self.assertEqual(oracle.main(["--scenario", "success"]), 0)
        self.assertLess(len(output.getvalue().encode()), oracle.MAX_OUTPUT_BYTES)
        parsed = obj(decode_json(output.getvalue()))
        self.assertEqual(parsed["schemaVersion"], 1)
        with self.assertRaisesRegex(ValueError, "oracle_scenario"):
            _ = oracle.run_scenario("unlisted")
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary).resolve()
            with self.assertRaisesRegex(ValueError, "fixture_directory"):
                _ = oracle.run_scenario("success", path)
        self.assertEqual(string(oracle.run_scenario("success"), "input", "today"), "2050-01-01")


if __name__ == "__main__":
    _ = unittest.main()
