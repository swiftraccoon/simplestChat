"""Closed whole-host receiver policy, real urllib and retention regressions."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from typing import TYPE_CHECKING, ClassVar, cast, override

import release_receive_oracle as oracle
from test_support import array, obj

# isort: split
import bounded_process
from release_fetch_receiver import CHUNK

if TYPE_CHECKING:
    from release_json import JsonObject


class WholeReceiveOracleTests(unittest.TestCase):
    """Exercise all closed operations with the unchanged receiver main."""

    observations: ClassVar[dict[str, JsonObject]] = {}

    @classmethod
    @override
    def setUpClass(cls) -> None:
        """Settle each actual isolated worker before retaining any test verdict."""
        for scenario in oracle.SCENARIOS:
            cls.observations[scenario] = oracle.run(scenario)

    def observation(self, scenario: str) -> JsonObject:
        """Read a complete already-settled whole-reference observation."""
        return obj(self.observations[scenario], "observation")

    def report(self, scenario: str) -> JsonObject:
        """Read the actual final receipt, when the original could emit it."""
        return obj(array(self.observation(scenario), "receipts")[-1])

    def files(self, scenario: str) -> JsonObject:
        """Read the first actual retained download attempt inventory."""
        return obj(array(self.observation(scenario), "attempts")[0], "files")

    def test_complete_closed_domain_and_worker_settlement(self) -> None:
        """Every finite case keeps original sources and all process peers settled."""
        self.assertEqual(len(oracle.SCENARIOS), 104)
        self.assertEqual(len(set(oracle.SCENARIOS)), 104)
        for name in oracle.SCENARIOS:
            with self.subTest(scenario=name):
                value = self.observation(name)
                self.assertEqual(value["passed"], name in oracle.POSITIVE)
                self.assertTrue(value["originalUnchanged"])
                self.assertTrue(value["sourceUnchanged"])
                self.assertTrue(value["originalPythonUnchanged"])
                self.assertGreaterEqual(cast("int", value["originalPythonCount"]), 267)
                self.assertTrue(value["originalBaselinePythonUnchanged"])
                self.assertEqual(value["originalBaselinePythonCount"], 267)
                self.assertEqual(len(obj(value, "originalBaselinePythonPins")), 267)
                self.assertTrue(value["lockReleased"])
                self.assertEqual(
                    value["workerSettled"],
                    {
                        "status": 0,
                        "pipesClosed": True,
                        "reaped": True,
                        "stderrBytes": 0,
                    },
                )
                self.assertNotIn(oracle.GRANT, json.dumps(self.observations[name]))

    def test_original_handshake_and_logical_megabyte_effects(self) -> None:
        """Actual ready flush precedes URL; only two reads and no EOF wait occur."""
        value = self.observation("wire-trailing")
        events = [obj(row) for row in array(value, "events")]
        inputs = [row for row in events if row["role"] == "input"]
        self.assertEqual([row["index"] for row in inputs], [1, 2])
        ready = next(
            index
            for index, row in enumerate(events)
            if row["role"] == "output" and row["final"] is False
        )
        second = next(
            index
            for index, row in enumerate(events)
            if row["role"] == "input" and row["index"] == oracle.FRAME_COUNT
        )
        self.assertLess(ready, second)
        reads = [row for row in events if row["role"] == "http-read"]
        self.assertEqual(reads[0]["bytes"], CHUNK)
        self.assertEqual(reads[0]["requested"], CHUNK)
        self.assertEqual(value["lockHeldAtReceipt"], [True])
        attempt = obj(array(value, "attempts")[0])
        self.assertEqual(attempt["publicationLinks"], {"release.json": True, "image.tar": True})
        links = [row["name"] for row in events if row["role"] == "link"]
        self.assertEqual(links, ["release.json", "image.tar"])

    def test_actual_urllib_errors_precede_target_creation(self) -> None:
        """NoRedirect and HTTPErrorProcessor remain before receiver target open."""
        for name in ("http-status-404", "http-status-302"):
            with self.subTest(scenario=name):
                self.assertEqual(self.report(name)["failureClass"], "http_error")
                self.assertNotIn("artifact.zip", self.files(name))
                closed = [
                    obj(row)
                    for row in array(self.observation(name), "events")
                    if obj(row)["role"] == "http-closed"
                ]
                self.assertTrue(closed)
                self.assertTrue(closed[-1]["pipeClosed"])
        for name in ("http-status-201", "http-ce-gzip", "http-cl-first-invalid"):
            self.assertEqual(obj(self.files(name), "artifact.zip")["bytes"], 0)
            self.assertEqual(self.report(name)["failureClass"], "http_response_rejected")

    def test_wire_error_classes_are_distinct(self) -> None:
        """Duplicate JSON and malformed syntax retain their actual coarse classes."""
        self.assertEqual(self.report("frame-duplicate")["failureClass"], "ambiguous_json")
        self.assertEqual(self.report("frame-syntax")["failureClass"], "validation_failed")

    def test_first_duplicate_headers_and_real_http_framing(self) -> None:
        """Message.get chooses the first duplicate; Content-Length hides trailing data."""
        for name in ("http-ce-first-valid", "http-cl-first-valid", "http-extra-ignored"):
            self.assertTrue(self.observation(name)["passed"])
        for name in ("http-ce-first-invalid", "http-cl-first-invalid"):
            self.assertFalse(self.observation(name)["passed"])
        self.assertEqual(self.report("http-overflow")["downloadedBytes"], CHUNK)
        self.assertEqual(self.report("http-truncated")["failureClass"], "download_size_mismatch")

    def test_actual_failure_prefixes_and_progress_are_separate(self) -> None:
        """An unsuccessful three-byte write is retained without advancing logical progress."""
        self.assertEqual(obj(self.files("http-write-error"), "artifact.zip")["bytes"], 3)
        self.assertEqual(self.report("http-write-error")["downloadedBytes"], 0)
        for name in ("http-read-error", "receiver-interrupt", "receiver-alarm"):
            self.assertEqual(obj(self.files(name), "artifact.zip")["bytes"], CHUNK)
            self.assertEqual(self.report(name)["downloadedBytes"], CHUNK)
        self.assertEqual(obj(self.files("extract-write-error"), "image.tar")["bytes"], 3)
        self.assertEqual(self.report("extract-write-error")["phase"], "extract")

    def test_late_evidence_and_semantic_predicate_domain(self) -> None:
        """Host semantic equality is retained while actual evidence checks reject bad records."""
        for name in ("predicate-semantic-equal", "id-i64", "frame-utf16-be", "ignored-large-int"):
            self.assertTrue(self.observation(name)["passed"])
        for name in (
            "source-revision",
            "source-keys",
            "outcome-error-missing",
            "manifest-timestamp",
            "bundle-mutated",
            "runtime-expired",
            "runtime-clock-error",
            "security-passed-int",
        ):
            self.assertEqual(self.report(name)["phase"], "verify")
            self.assertFalse(self.observation(name)["passed"])
        self.assertEqual(self.report("id-i64")["artifactId"], 2**63 - 1)
        first = obj(self.observations["frame-exact-limit"], "input", "frames", "envelope.bin")
        self.assertEqual(first["bytes"], 16384)
        self.assertTrue(self.observation("frame-exact-limit")["passed"])
        self.assertTrue(self.observation("url-host-domain")["passed"])

    def test_publication_prechecks_repeat_identity_and_prefix(self) -> None:
        """Both existing targets are checked before either effect; equal repeats preserve inodes."""
        for name in ("existing-manifest-diff", "existing-image-diff", "existing-symlink"):
            links = [
                obj(row)
                for row in array(self.observation(name), "events")
                if obj(row)["role"] == "link"
            ]
            self.assertEqual(links, [])
        self.assertEqual(
            set(obj(self.observation("second-link-error"), "publication")), {"release.json"}
        )
        self.assertEqual(self.report("second-link-error")["finalized"], False)
        attempts = array(self.observation("repeat"), "attempts")
        self.assertEqual(len(attempts), 2)
        relations = [obj(row)["publicationLinks"] for row in attempts]
        self.assertCountEqual(
            relations,
            [
                {"release.json": True, "image.tar": True},
                {"release.json": False, "image.tar": False},
            ],
        )
        self.assertTrue(self.observation("race-eexist-equal")["passed"])
        same = obj(array(self.observation("race-eexist-hardlink"), "attempts")[0])
        foreign = obj(array(self.observation("race-eexist-equal"), "attempts")[0])
        self.assertTrue(obj(same, "publicationLinks")["release.json"])
        self.assertFalse(obj(foreign, "publicationLinks")["release.json"])

    def test_receipt_faults_preserve_published_evidence_and_original_escape(self) -> None:
        """Receipt persistence failure cannot withdraw earlier hardlinks or invent settlement."""
        for name in ("receipt-create-error", "receipt-fsync-error"):
            report = self.report(name)
            self.assertEqual(report["failureClass"], "receipt_write_failed")
            self.assertFalse(report["settled"])
            self.assertTrue(report["finalized"])
            self.assertEqual(
                set(obj(self.observation(name), "publication")), {"release.json", "image.tar"}
            )
        for name in ("final-write-error", "final-flush-error"):
            self.assertEqual(self.observation(name)["uncaughtClasses"], ["OSError"])
            self.assertEqual(len(array(self.observation(name), "attempts")), 1)
            self.assertEqual(self.observation(name)["lockHeldAtReceipt"], [True])

    def test_pristine_export_has_explicit_private_modes_and_separate_reference(self) -> None:
        """Native roots are untouched by reference attempts and fixture binaries stay private."""
        with tempfile.TemporaryDirectory(prefix="receive-export.") as name:
            root = Path(name).resolve()
            result = oracle.run("success", root)
            native = root / "native"
            self.assertEqual(list((native / "public" / "releases" / oracle.REVISION).iterdir()), [])
            self.assertEqual((native / "bin" / "curl").stat().st_mode & 0o777, 0o700)
            for path in (native / "inputs").iterdir():
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertTrue(obj(result, "observation")["passed"])
            self.assertNotEqual(native, root / "reference")

    def test_baseline_source_subset_cannot_be_replaced_by_extra_maintained_files(self) -> None:
        """Aggregate growth cannot hide a missing original source name or byte pin."""
        current = oracle.original_python_pins()
        baseline = oracle.baseline_python_pins(current)
        self.assertEqual(len(baseline), 267)
        self.assertEqual(set(baseline), set(oracle.BASELINE_PYTHON_PATHS))
        extra = dict(current)
        extra["build/new-future-proof.py"] = {"sha256": "a" * 64, "bytes": 1, "mode": 384}
        self.assertEqual(oracle.baseline_python_pins(extra), baseline)
        removed = dict(extra)
        del removed[oracle.BASELINE_PYTHON_PATHS[0]]
        with self.assertRaises(ValueError):
            _ = oracle.baseline_python_pins(removed)
        changed = dict(current)
        changed[oracle.BASELINE_PYTHON_PATHS[0]] = {"sha256": "b" * 64, "bytes": 1, "mode": 384}
        self.assertNotEqual(oracle.baseline_python_pins(changed), baseline)

    def test_native_empty_ca_directory_matches_actual_attempt_shape(self) -> None:
        """Per-attempt empty trust directories stay checked rather than ignored arguments."""
        with tempfile.TemporaryDirectory(prefix="receive-capath.") as name:
            root = Path(name).resolve()
            source = oracle.fixture("success")
            _ = oracle.populate(root, source, "success")
            attempt = root / "public" / "releases" / oracle.REVISION / "download.owned"
            oracle.directory(attempt)
            ca = attempt / ".curl-ca.owned"
            oracle.directory(ca)
            self.assertTrue(oracle.valid_capath(root, ca))
            oracle.write(ca / "foreign", b"inert foreign CA")
            self.assertFalse(oracle.valid_capath(root, ca))
            self.assertFalse(oracle.valid_capath(root, root / "config"))

    def test_native_adapter_matches_content_length_observations(self) -> None:
        """Fixed inert curl projects real curl's bounded body and truncated status18."""
        for name, expected in (("http-extra-ignored", 0), ("http-truncated", 18)):
            with self.subTest(scenario=name), tempfile.TemporaryDirectory() as directory:
                root = Path(directory).resolve()
                source = oracle.fixture(name)
                _ = oracle.populate(root, source, name)
                arguments = oracle.curl_arguments(root)
                config = oracle.curl_config(root)
                status, data, error = bounded_process.run(
                    [str(root / "bin" / "curl"), *arguments],
                    limits=bounded_process.Limits(timeout=5, stdout=2 * 1024**2, stderr=65536),
                    input_data=config,
                    env={"PATH": "/usr/bin:/bin", "PYTHONDONTWRITEBYTECODE": "1"},
                )
                self.assertEqual(status, expected)
                self.assertEqual(error, b"")
                body = data.split(b"\r\n\r\n", 1)[1]
                expected_body = source.files["artifact.zip"]
                self.assertEqual(
                    body,
                    expected_body if expected == 0 else expected_body[: CHUNK + 3],
                )


if __name__ == "__main__":
    _ = unittest.main()
