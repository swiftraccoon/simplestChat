"""Closed whole-controller/receiver policy, effect and fixture-adapter regressions."""

from __future__ import annotations

import io
import os
import signal
import subprocess
import tempfile
import unittest
from pathlib import Path
from typing import ClassVar, cast, override
from unittest.mock import patch

import release_fetch_controller_oracle as oracle
from test_support import array, obj

# isort: split
import release_trust as trust
from release_json import JsonObject, decode_json


class FinalWireDeliveryTests(unittest.TestCase):
    """Make immediate receipt rejection independent of the worker's scheduling."""

    def test_actual_worker_records_lock_release_before_delivering_invalid_receipt(self) -> None:
        """A synchronous rejecting consumer cannot stop the final lock observation."""
        with tempfile.TemporaryDirectory(prefix="fetch-final-wire.") as name:
            root = Path(name).resolve()
            source = oracle.fixture("final-identity-mismatch")
            control = oracle.populate(root, source)
            _ = oracle.prepare_host(root, source)
            envelope = dict(source.selection)
            envelope["attestation"] = {
                "predicate": decode_json(source.files[trust.PREDICATE]),
                "bundleSha256": oracle.digest(source.files[trust.BUNDLE]),
                "verificationSha256": dict.fromkeys(trust.SUBJECTS, "c" * 64),
            }

            class Consumer(io.StringIO):
                """Observe the real final-pipe write immediately, before it can return."""

                @override
                def write(self, text: str) -> int:
                    """Require the independent completed lock observation before final bytes."""
                    value = obj(decode_json(text))
                    if value.get("status") != "ready":
                        events = [
                            obj(decode_json(line))
                            for line in (root / "events.jsonl").read_bytes().splitlines()
                        ]
                        finished = [row for row in events if row["role"] == "ssh-finished"]
                        if len(finished) != 1 or finished[0]["lockAvailableAfter"] is not True:
                            message = "final wire preceded the real worker lock observation"
                            raise AssertionError(message)
                    return super().write(text)

            wire = oracle.encoded(envelope) + oracle.encoded({"url": oracle.REMOTE_GRANT})
            handlers = {
                number: signal.getsignal(number)
                for number in (signal.SIGINT, signal.SIGTERM, signal.SIGALRM)
            }
            previous_mask = os.umask(0o077)
            try:
                with (
                    io.TextIOWrapper(io.BytesIO(wire)) as input_stream,
                    Consumer() as output,
                    patch("sys.stdin", input_stream),
                    patch("sys.stdout", output),
                    patch.dict(os.environ, {}, clear=True),
                ):
                    status = oracle.worker(root, [str(v) for v in array(control, "sshArguments")])
                    values = [obj(decode_json(line)) for line in output.getvalue().splitlines()]
            finally:
                for number, handler in handlers.items():
                    _ = signal.signal(number, handler)
                _ = os.umask(previous_mask)
            self.assertEqual(status, 0)
            self.assertEqual([row["status"] for row in values], ["ready", "complete"])
            self.assertEqual(values[-1]["artifactId"], 124)
            original = obj(decode_json((root / "remote-final-000.json").read_bytes()))
            self.assertEqual(original["artifactId"], 123)


class WholeFetchOracleTests(unittest.TestCase):
    """Exercise every closed case with actual controller and receiver orchestration."""

    observations: ClassVar[dict[str, JsonObject]] = {}

    @classmethod
    @override
    def setUpClass(cls) -> None:
        """Finish every owned child before retaining each complete finite observation."""
        for scenario in oracle.SCENARIOS:
            with tempfile.TemporaryDirectory(prefix="fetch-whole-oracle.") as name:
                cls.observations[scenario] = oracle.run(scenario, Path(name).resolve())

    def observation(self, name: str) -> JsonObject:
        """Read one already-settled observation without repeating runtime work."""
        return obj(self.observations[name], "observation")

    def report(self, name: str) -> JsonObject:
        """Read the final actual controller report after all reference finalizers."""
        return obj(array(self.observation(name), "runs")[-1], "report")

    def roles(self, name: str) -> list[str]:
        """Project only the finite safe event role vocabulary."""
        return [str(obj(row)["role"]) for row in array(self.observation(name), "events")]

    def test_all_closed_cases_preserve_sources_and_settle_actual_children(self) -> None:
        """Keep the complete pass/fail domain and every real saved process explicit."""
        self.assertEqual(len(oracle.SCENARIOS), 73)
        self.assertEqual(len(set(oracle.SCENARIOS)), len(oracle.SCENARIOS))
        for name in oracle.SCENARIOS:
            with self.subTest(scenario=name):
                value = self.observation(name)
                self.assertEqual(value["passed"], name in oracle.POSITIVE)
                self.assertTrue(value["sourceUnchanged"])
                self.assertTrue(value["originalPythonUnchanged"])
                self.assertGreaterEqual(cast("int", value["originalPythonCount"]), 258)
                self.assertTrue(value["secretsAbsent"])
                self.assertEqual(value["stderrBytes"], 0)
                for child in array(value, "children"):
                    self.assertTrue(obj(child)["reaped"])
                    self.assertTrue(obj(child)["pipesClosed"])

    def test_success_uses_both_original_mains_and_two_distinct_grants(self) -> None:
        """Metadata/crypto precede SSH; the remote grant follows actual locked readiness."""
        roles = self.roles("success-fresh")
        for name in (
            "metadata-artifact",
            "metadata-run",
            "grant-local",
            "local-http",
            "ssh-start",
            "receiver-ready",
            "grant-remote",
            "remote-http",
        ):
            self.assertIn(name, roles)
        self.assertEqual(roles.count("token"), 2)
        self.assertEqual(roles.count("verify"), 4)
        self.assertLess(roles.index("grant-local"), roles.index("ssh-start"))
        self.assertLess(roles.index("receiver-ready"), roles.index("grant-remote"))
        ended = obj(array(self.observation("success-fresh"), "events")[-1])
        self.assertEqual(ended["lockHeldAtReceipt"], [True])
        self.assertTrue(ended["lockAvailableAfter"])
        attempt = obj(array(self.observation("success-fresh"), "attempts")[0])
        self.assertEqual(attempt["publicationLinks"], {"release.json": True, "image.tar": True})

    def test_cached_and_repeat_keep_fresh_crypto_without_local_download(self) -> None:
        """Cached does two API reads and four fresh signatures each time, then one remote URL."""
        roles = self.roles("repeat-cached")
        self.assertNotIn("grant-local", roles)
        self.assertNotIn("local-http", roles)
        self.assertEqual(roles.count("metadata-artifact"), 2)
        self.assertEqual(roles.count("metadata-run"), 2)
        self.assertEqual(roles.count("verify"), 8)
        self.assertEqual(roles.count("grant-remote"), 2)
        self.assertEqual(len(array(self.observation("repeat-cached"), "attempts")), 2)
        self.assertTrue(self.observation("repeat-cached")["originalUnchanged"])

    def test_refusals_preserve_actual_local_before_remote_phase_order(self) -> None:
        """Malformed ZIP and failed local signatures never manufacture receiver work."""
        for name in (
            "metadata-refusal",
            "signature-first",
            "signature-fourth",
            "cached-member-changed",
            *[n for n in oracle.SCENARIOS if n.startswith("zip-")],
        ):
            with self.subTest(scenario=name):
                self.assertNotIn("ssh-start", self.roles(name))
                self.assertEqual(self.observation(name)["attempts"], [])
        for name in ("root-mode", "lock-busy", "work-journal-unfinished"):
            self.assertIn("ssh-start", self.roles(name))
            self.assertNotIn("receiver-ready", self.roles(name))

    def test_http_first_duplicate_and_partial_prefix_effects_are_real(self) -> None:
        """Receiver's Message.get accepts the first duplicate; failures retain actual prefixes."""
        for name in ("http-status", "http-ce-gzip", "http-cl-duplicate-first-invalid"):
            files = obj(array(self.observation(name), "attempts")[0], "files")
            self.assertEqual(obj(files, "artifact.zip")["bytes"], 0)
        for name in ("http-read-fault", "receiver-interrupt", "receiver-alarm"):
            files = obj(array(self.observation(name), "attempts")[0], "files")
            self.assertEqual(obj(files, "artifact.zip")["bytes"], oracle.CHUNK)
        files = obj(array(self.observation("http-write-fault"), "attempts")[0], "files")
        self.assertEqual(obj(files, "artifact.zip")["bytes"], 3)
        self.assertEqual(
            obj(self.observation("http-write-fault"), "receiverFinal")["downloadedBytes"], 0
        )

    def test_late_host_guards_and_hardlink_prefix_never_overwrite(self) -> None:
        """Late file bindings run on the host; second-link failure retains only the first link."""
        for name in (
            "source-mutated",
            "outcome-failed",
            "manifest-revision",
            "bundle-mutated",
            "runtime-expired",
            "runtime-utc-error",
        ):
            with self.subTest(scenario=name):
                self.assertIn("receiver-ready", self.roles(name))
                self.assertEqual(obj(self.observation(name), "receiverFinal")["phase"], "verify")
        first = obj(array(self.observation("first-link-error"), "attempts")[0])
        second = obj(array(self.observation("second-link-error"), "attempts")[0])
        self.assertEqual(first["publicationLinks"], {"release.json": False, "image.tar": False})
        self.assertEqual(second["publicationLinks"], {"release.json": True, "image.tar": False})
        for name in ("existing-manifest-diff", "existing-image-diff"):
            self.assertEqual(
                obj(self.observation(name), "receiverFinal")["failureClass"],
                "retained_artifact_differs",
            )

    def test_receipt_settlement_does_not_authorize_unknown_transport(self) -> None:
        """Valid publication survives uncertain wire outcomes; remoteSettled follows finish only."""
        for name in (
            "final-missing",
            "final-truncated",
            "final-extra-output",
            "final-non-ascii-whitespace",
            "final-identity-mismatch",
        ):
            with self.subTest(scenario=name):
                self.assertFalse(self.report(name)["remoteSettled"])
                final = obj(self.observation(name), "receiverFinal")
                self.assertTrue(final["passed"])
                self.assertTrue(final["settled"])
        self.assertTrue(self.report("final-nonzero")["remoteSettled"])
        self.assertFalse(self.report("final-nonzero")["passed"])
        self.assertTrue(self.report("final-ascii-whitespace")["passed"])
        for name in ("receipt-write-error", "receipt-fsync-error"):
            final = obj(self.observation(name), "receiverFinal")
            self.assertEqual(final["failureClass"], "receipt_write_failed")
            self.assertTrue(final["finalized"])
            self.assertFalse(final["settled"])

    def test_empty_cached_selector_remains_an_explicit_native_profile(self) -> None:
        """Original empty string means cached cwd, with no local URL or download."""
        value = self.observations["cached-empty-directory"]
        self.assertEqual(obj(value, "input")["nativeProfile"], "canonical-cached-directory")
        self.assertTrue(self.observation("cached-empty-directory")["passed"])
        self.assertNotIn("local-http", self.roles("cached-empty-directory"))

    def test_every_local_attempt_keeps_its_full_final_report_and_artifact_witness(self) -> None:
        """Keep local journals and file modes visible even when remote work never began."""
        before_artifact = {
            "metadata-refusal",
            "signature-first",
            "signature-fourth",
            "cached-member-changed",
            *[name for name in oracle.SCENARIOS if name.startswith("zip-")],
        }
        for name in oracle.SCENARIOS:
            with self.subTest(scenario=name):
                value = self.observation(name)
                controller = obj(value, "controllerEvidence")
                self.assertTrue(controller["exists"])
                self.assertEqual(controller["mode"], 0o700)
                attempts = [obj(item) for item in array(controller, "attempts")]
                reports = [obj(item, "report") for item in array(value, "runs")]
                self.assertEqual(len(attempts), len(reports))
                outcomes: list[JsonObject] = []
                for attempt in attempts:
                    self.assertEqual(obj(attempt, "directories")[""], 0o700)
                    journals = obj(attempt, "journals")
                    outcome = obj(journals, "outcome.json")
                    self.assertEqual(outcome["mode"], 0o600)
                    outcomes.append(obj(outcome, "value"))
                    self.assertEqual("artifact.json" not in journals, name in before_artifact)
                    if "artifact.json" in journals:
                        artifact = obj(journals, "artifact.json")
                        self.assertEqual(artifact["mode"], 0o600)
                        self.assertEqual(obj(artifact, "value")["revision"], oracle.REVISION)
                        self.assertIn("attestation", obj(artifact, "value"))
                self.assertCountEqual(outcomes, reports)


class CensusAndLocalEvidenceTests(unittest.TestCase):
    """Refuse blind spots without observing other tests' disposable output trees."""

    def test_census_keeps_maintained_sources_and_ignores_transient_root_outputs(self) -> None:
        """Transient fixtures stay outside the census; changed maintained bytes remain detected."""
        with tempfile.TemporaryDirectory(prefix="fetch-census.") as name:
            root = Path(name).resolve()
            maintained = {
                *(f"{prefix}/fixture.py" for prefix in oracle.PYTHON_ROOTS),
                "xtask/src/ci/release_http_policy/reference/original.py",
                *oracle.PYTHON_VENDOR_PATHS,
            }
            transient = {
                f"{prefix}/fixture.py"
                for prefix in ("output", "outputs", "results", "target", ".cache")
            }
            for relative in maintained | transient | oracle.OWN_PYTHON_PATHS:
                path = root / relative
                path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
                oracle.write(path, b"original = True\n")
            with patch.object(oracle, "ROOT", root):
                before = oracle.python_pins()
                for relative in transient:
                    _ = (root / relative).write_bytes(b"changed = True\n")
                (root / "results/fixture.py").unlink()
                oracle.write(root / "results/second.py", b"new = True\n")
                after_transient = oracle.python_pins()
                _ = (root / "build/fixture.py").write_bytes(b"changed = True\n")
                after_source = oracle.python_pins()
        self.assertEqual(set(before), maintained)
        self.assertEqual(after_transient, before)
        self.assertNotEqual(after_source, before)
        self.assertEqual(
            {key for key in before if before[key] != after_source[key]}, {"build/fixture.py"}
        )

    def test_local_observer_keeps_semantic_values_modes_and_missing_journals_distinct(self) -> None:
        """A changed or removed journal cannot survive as an equivalent local witness."""
        with tempfile.TemporaryDirectory(prefix="fetch-local-evidence.") as name:
            root = Path(name).resolve()
            attempt = root / "controller/release-fetch.inert"
            attempt.mkdir(mode=0o700, parents=True)
            artifact = attempt / "artifact.json"
            outcome = attempt / "outcome.json"
            oracle.write(artifact, b'{"artifactId":123}\n')
            oracle.write(outcome, b'{"passed":false,"phase":"attestation"}\n')
            before = oracle.controller_evidence(root)
            _ = outcome.write_bytes(b'{"passed":true,"phase":"attestation"}\n')
            changed_value = oracle.controller_evidence(root)
            outcome.chmod(0o400)
            changed_mode = oracle.controller_evidence(root)
            artifact.unlink()
            missing_artifact = oracle.controller_evidence(root)
        self.assertNotEqual(before, changed_value)
        self.assertNotEqual(changed_value, changed_mode)
        self.assertNotEqual(changed_mode, missing_artifact)
        self.assertEqual(
            obj(array(obj(changed_value), "attempts")[0], "journals")["outcome.json"],
            {"mode": 0o600, "value": {"passed": True, "phase": "attestation"}},
        )


class ExportedBridgeTests(unittest.TestCase):
    """Validate actual exported executable adapters without invoking a real tool."""

    def test_export_is_pristine_and_fixed_ssh_rejects_an_unrecognized_command(self) -> None:
        """The reference effects stay separate and the bridge cannot become a shell."""
        with tempfile.TemporaryDirectory(prefix="fetch-export.") as name:
            root = Path(name).resolve()
            result = oracle.export_and_run("success-fresh", root)
            native = root / "native"
            pristine = oracle.inventory(native / "candidate")
            process = subprocess.run(  # noqa: S603 -- Test-owned fixed executable, no shell.
                [str(native / "bin" / "ssh"), "unrecognized-command"],
                stdin=subprocess.DEVNULL,
                capture_output=True,
                timeout=10,
                check=False,
                env={"PATH": "/usr/bin:/bin"},
            )
            current = oracle.inventory(native / "candidate")
            controller_absent = not (native / "controller").exists()
            attempts = list((native / "remote" / "public").glob("releases/*/download.*"))
            closure = obj(decode_json((root / "source-python.json").read_bytes()))
        self.assertEqual(process.returncode, 99)
        self.assertEqual(process.stdout, b"")
        self.assertEqual(process.stderr, b"")
        self.assertEqual(current, pristine)
        self.assertTrue(controller_absent)
        self.assertEqual(attempts, [])
        self.assertTrue(obj(result, "observation")["passed"])
        self.assertGreaterEqual(len(closure), 258)

    def test_exported_curl_keeps_both_grants_private(
        self,
    ) -> None:
        """Fixed native HTTP adapter returns local then remote grant through stdin-only config."""
        outputs: list[subprocess.CompletedProcess[bytes]] = []
        with tempfile.TemporaryDirectory(prefix="fetch-adapter.") as name:
            root = Path(name).resolve()
            source = oracle.fixture("success-fresh")
            _ = oracle.populate(root, source)
            control = obj(decode_json((root / "adapter.json").read_bytes()))
            configuration = (
                b'url = "https://api.github.com/"\n'
                b'request-target = "/repos/owner/repo/actions/artifacts/123/zip"\n'
                b'user-agent = "simplestchat-release-fetch"\nconnect-timeout = "20"\n'
                b'header = "Authorization: Bearer INERT_FETCH_CONTROLLER_TOKEN"\n'
                b'header = "Accept: application/vnd.github+json"\n'
            )
            for _index in range(2):
                outputs.append(  # noqa: PERF401 -- Each call changes the owned role journal.
                    subprocess.run(  # noqa: S603 -- Test-owned fixed executable, no shell.
                        [
                            str(root / "bin" / "curl"),
                            *[str(v) for v in array(control, "curlArguments")],
                        ],
                        input=configuration,
                        capture_output=True,
                        timeout=10,
                        check=False,
                        env={"PATH": "/usr/bin:/bin"},
                    )
                )
            events = (root / "native-events.jsonl").read_bytes()
        self.assertEqual([p.returncode for p in outputs], [0, 0])
        self.assertIn(oracle.LOCAL_GRANT.encode(), outputs[0].stdout)
        self.assertIn(oracle.REMOTE_GRANT.encode(), outputs[1].stdout)
        self.assertNotIn(oracle.TOKEN, events)
        self.assertNotIn(b"INERT_LOCAL_GRANT", events)
        self.assertNotIn(b"INERT_REMOTE_GRANT", events)

    def test_invalid_cli_selector_has_no_fixture_effects(self) -> None:
        """Argparse's closed scenario set refuses before any directory creation."""
        with tempfile.TemporaryDirectory(prefix="fetch-cli.") as name:
            target = Path(name).resolve() / "never-created"
            output, diagnostic = io.StringIO(), io.StringIO()
            with (
                patch("sys.stdout", output),
                patch("sys.stderr", diagnostic),
                self.assertRaises(SystemExit) as error,
            ):
                _ = oracle.main(["--scenario", "arbitrary", "--fixture-directory", str(target)])
            absent = not target.exists()
        self.assertEqual(error.exception.code, 2)
        self.assertTrue(absent)


if __name__ == "__main__":
    _ = unittest.main()
