"""Exact release identity and signature-bound evidence tests, without remote operations."""

from __future__ import annotations

import copy
import hashlib
import json
import sys
import tempfile
import unittest
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import TYPE_CHECKING
from unittest.mock import patch

from test_release_fetch import envelope, fixture_files, make_zip
from test_support import ROOT

# isort: split
import bounded_process
import release_attestation as attest
import release_fetch_receiver as receiver
import release_trust as trust
import security_archive
from release_json import JsonObject, JsonValue, decode_json, object_value

if TYPE_CHECKING:
    from collections.abc import Sequence

_ = ROOT


def fixture(directory: Path) -> tuple[JsonObject, JsonObject]:
    """Write inert current artifacts; fake bundle bytes are never accepted by real gh."""
    for name, data in fixture_files().items():
        _ = (directory / name).write_bytes(data)
    data = make_zip()
    _ = (directory / "artifact.zip").write_bytes(data)
    selected = envelope(data)
    claim = object_value(object_value(selected["attestation"])["predicate"])
    return selected, claim


def verified_output(claim: JsonObject) -> list[JsonValue]:
    """Model the documented gh verificationResult shape, never a raw decoded DSSE payload."""
    repo, revision = claim["repository"], claim["revision"]
    certificate: JsonObject = {
        "subjectAlternativeName": f"https://github.com/{repo}/{trust.WORKFLOW}@{trust.REF}",
        "issuer": trust.ISSUER,
        "buildSignerDigest": revision,
        "sourceRepositoryURI": f"https://github.com/{repo}",
        "sourceRepositoryDigest": revision,
        "sourceRepositoryRef": trust.REF,
        "runnerEnvironment": "github-hosted",
        "buildTrigger": "push",
        "runInvocationURI": f"https://github.com/{repo}/actions/runs/{claim['runId']}/attempts/{claim['runAttempt']}",
    }
    files = object_value(claim["fileDigests"])
    return [
        {
            "verificationResult": {
                "signature": {"certificate": certificate},
                "verifiedTimestamps": [{"type": "Tlog", "timestamp": "2026-09-30T00:00:00Z"}],
                "statement": {
                    "_type": "https://in-toto.io/Statement/v1",
                    "predicateType": trust.PREDICATE_TYPE,
                    "predicate": claim,
                    "subject": [
                        {"name": name, "digest": {"sha256": files[name]}} for name in trust.SUBJECTS
                    ],
                },
            }
        }
    ]


def runtime_fixture(directory: Path) -> tuple[JsonObject, JsonObject, JsonObject]:
    """Model a signed conditional disposition for receiver relationship checks."""
    _, claim = fixture(directory)
    security = trust.read(directory / "image-security.json")
    proof = trust.read(directory / "runtime-proof.json")
    proof.update(
        required=True,
        scope="managed-default-server-dtls",
        sourceReview={"expires": (datetime.now(UTC).date() + timedelta(days=30)).isoformat()},
    )
    for key in ("binarySha256", "nativeSha256", "elfSha256", "sbomSha256"):
        proof[key] = security.setdefault(key, "b" * 64)
    security["vexRequired"] = True
    _ = (directory / "vex.openvex.json").write_text('{"statements":[{"status":"not_affected"}]}')
    publish_runtime_fixture(directory, proof, security, claim)
    return proof, security, claim


def publish_runtime_fixture(
    directory: Path, proof: JsonObject, security: JsonObject, claim: JsonObject
) -> None:
    """Authenticate deliberately selected test bytes before checking their relationships."""
    _ = (directory / "runtime-proof.json").write_text(json.dumps(proof))
    for key, name in (
        ("runtimeProofSha256", "runtime-proof.json"),
        ("vexSha256", "vex.openvex.json"),
    ):
        checksum = hashlib.sha256((directory / name).read_bytes()).hexdigest()
        security[key] = checksum
        object_value(claim["fileDigests"])[name] = checksum


class ReleaseAttestationTests(unittest.TestCase):
    """Authenticity, source identity and content bindings are independent requirements."""

    def test_real_cli_imports_need_no_test_path_bootstrap(self) -> None:
        """Ordinary checkout entrypoints can show help without credentials or remote work."""
        for name in ("release_attestation.py", "verify-release.py"):
            with self.subTest(name=name):
                status, output, error = bounded_process.run(
                    [sys.executable, str(ROOT / "build" / name), "--help"],
                    limits=bounded_process.Limits(timeout=5, stdout=65536, stderr=65536),
                    env={"PATH": "/usr/bin:/bin", "LC_ALL": "C"},
                )
                self.assertEqual(status, 0, error.decode())
                self.assertIn(b"usage:", output)

    def test_producer_requires_success_and_binds_exact_original_export(self) -> None:
        """The trusted signer receives one complete predicate without rebuilding or resaving."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            artifact, security = root / "artifact", root / "security"
            artifact.mkdir()
            (security / "spdx").mkdir(parents=True)
            files = fixture_files()
            for name in ("image.tar", "release.json", "outcome.json", "source.json"):
                _ = (artifact / name).write_bytes(files[name])
            _ = (security / "spdx/sbom.spdx.json").write_bytes(files["sbom.spdx.json"])
            image_id = security_archive.image_identity(artifact / "image.tar")
            build = object_value(decode_json(files["outcome.json"]))
            build["exportedImageId"] = image_id
            _ = (artifact / "outcome.json").write_text(json.dumps(build))
            result = object_value(decode_json(files["image-security.json"]))
            result.update(
                imageId=image_id,
                selectedImage={"archiveConfigSha256": image_id.removeprefix("sha256:")},
                secretDetectorSelfTest=True,
                checks={
                    name: {"passed": True} for name in ("vulnerabilities", "licenses", "secrets")
                },
            )
            runtime = object_value(decode_json(files["runtime-proof.json"]))
            runtime["imageId"] = image_id
            runtime_bytes = json.dumps(runtime).encode()
            _ = (security / "runtime-proof.json").write_bytes(runtime_bytes)
            _ = (security / "vex.openvex.json").write_bytes(files["vex.openvex.json"])
            result["runtimeProofSha256"] = hashlib.sha256(runtime_bytes).hexdigest()
            for field, name in (
                ("nativeSha256", "native.json"),
                ("elfSha256", "elf.json"),
                ("secretPathMapSha256", "secret-paths.json"),
                ("databaseEvidenceSha256", "database-status.json"),
                ("runtimeLicenseEvidenceSha256", "runtime-license-evidence.json"),
            ):
                _ = (security / name).write_bytes(b"{}")
                result[field] = hashlib.sha256(b"{}").hexdigest()
            _ = (security / "outcome.json").write_text(json.dumps(result))
            for changed in ("imageId", "selectedImage"):
                invalid = copy.deepcopy(result)
                invalid[changed] = (
                    "sha256:" + "f" * 64
                    if changed == "imageId"
                    else {"archiveConfigSha256": "f" * 64}
                )
                _ = (security / "outcome.json").write_text(json.dumps(invalid))
                with (
                    self.subTest(changed=changed),
                    self.assertRaisesRegex(trust.TrustError, "config_identity"),
                ):
                    _ = attest.prepare(artifact, security, envelope())
                self.assertFalse((artifact / trust.PREDICATE).exists())
            _ = (security / "outcome.json").write_text(json.dumps(result))
            claim = attest.prepare(artifact, security, envelope())
            self.assertEqual(set(object_value(claim["fileDigests"])), set(trust.BOUND_FILES))
            self.assertEqual(trust.read(artifact / trust.PREDICATE), claim)
            self.assertFalse((artifact / trust.BUNDLE).exists())
            trust.bind_files(artifact, claim)

    def test_failed_image_evidence_cannot_be_prepared_for_signing(self) -> None:
        """No successful signature predicate can be created from a failed scanner outcome."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            selected, _ = fixture(root)
            security = root / "security"
            security.mkdir()
            _ = (security / "outcome.json").write_text('{"passed":false}')
            with self.assertRaisesRegex(trust.TrustError, "security_failed"):
                _ = attest.prepare(root, security, selected)

    def test_conditional_disposition_requires_matching_artifact_and_evidence(self) -> None:
        """Even signed bytes cannot mix another image or contradictory disposition state."""
        changes: tuple[tuple[str, JsonValue], ...] = (
            ("required", False),
            ("passed", False),
            ("imageId", "sha256:" + "1" * 64),
            ("archiveSha256", "1" * 64),
            ("revision", "1" * 40),
            ("platform", "linux/arm64"),
            ("binarySha256", "1" * 64),
            ("nativeSha256", "1" * 64),
            ("elfSha256", "1" * 64),
            ("sbomSha256", "1" * 64),
            ("scope", "all-programs"),
        )
        for key, value in changes:
            with self.subTest(key=key), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                proof, security, claim = runtime_fixture(directory)
                trust.bind_runtime(directory, security, claim)
                proof[key] = value
                publish_runtime_fixture(directory, proof, security, claim)
                with self.assertRaisesRegex(trust.TrustError, "runtime_"):
                    trust.bind_runtime(directory, security, claim)

    def test_expired_signed_runtime_review_is_rejected_at_receipt_time(self) -> None:
        """The review expiration date is exclusive and applies after signing as well."""
        today = datetime.now(UTC).date()
        for expires in (today.isoformat(), (today - timedelta(days=1)).isoformat(), "20990101"):
            with self.subTest(expires=expires), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                proof, security, claim = runtime_fixture(directory)
                object_value(proof["sourceReview"])["expires"] = expires
                publish_runtime_fixture(directory, proof, security, claim)
                with self.assertRaisesRegex(trust.TrustError, "review_expired"):
                    trust.bind_runtime(directory, security, claim)

    def test_conditional_disposition_requires_its_signed_nonempty_vex(self) -> None:
        """No required proof can be paired with an empty or changed disposition file."""
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            proof, security, claim = runtime_fixture(directory)
            security["vexSha256"] = "1" * 64
            with self.assertRaisesRegex(trust.TrustError, "runtime_hash_binding"):
                trust.bind_runtime(directory, security, claim)
            _ = (directory / "vex.openvex.json").write_text('{"statements":[]}')
            publish_runtime_fixture(directory, proof, security, claim)
            with self.assertRaisesRegex(trust.TrustError, "runtime_disposition"):
                trust.bind_runtime(directory, security, claim)

    def test_verifier_explicitly_enforces_trusted_workflow_and_source(self) -> None:
        """No permissive identity regex, self-hosted runner or unsigned fallback is offered."""
        selected = envelope()
        arguments = attest.verifier_arguments(Path("image.tar"), Path(trust.BUNDLE), selected)
        for flag, expected in (
            ("--repo", "owner/repo"),
            ("--hostname", "github.com"),
            ("--source-digest", "a" * 40),
            ("--signer-digest", "a" * 40),
            ("--source-ref", trust.REF),
            ("--cert-oidc-issuer", trust.ISSUER),
            ("--predicate-type", trust.PREDICATE_TYPE),
        ):
            self.assertEqual(arguments[arguments.index(flag) + 1], expected)
        self.assertIn("--deny-self-hosted-runners", arguments)
        self.assertNotIn("--cert-identity-regex", arguments)

    def test_current_claim_and_certificate_must_agree_on_run_attempt_and_subjects(self) -> None:
        """A valid signature from another run, branch, workflow or input is insufficient."""
        selected = envelope()
        claim = object_value(object_value(selected["attestation"])["predicate"])
        good = verified_output(claim)
        attest.verified_statement(good, selected, claim)
        paths: tuple[tuple[tuple[str, ...], JsonValue], ...] = (
            (
                ("signature", "certificate", "runInvocationURI"),
                "https://github.com/owner/repo/actions/runs/789/attempts/2",
            ),
            (("signature", "certificate", "sourceRepositoryDigest"), "b" * 40),
            (("signature", "certificate", "issuer"), "https://untrusted.example"),
            (("signature", "certificate", "runnerEnvironment"), "self-hosted"),
            (("signature", "certificate", "buildTrigger"), "pull_request"),
            (("statement", "predicateType"), "https://unrelated.example/predicate"),
            (("statement", "subject"), []),
            (("verifiedTimestamps",), []),
        )
        for path, replacement in paths:
            changed = copy.deepcopy(good)
            target = object_value(object_value(changed[0])["verificationResult"])
            for key in path[:-1]:
                target = object_value(target[key])
            target[path[-1]] = replacement
            with self.subTest(path=path), self.assertRaises(trust.TrustError):
                attest.verified_statement(changed, selected, claim)

    def test_empty_unverified_or_ambiguous_outputs_are_not_signature_evidence(self) -> None:
        """Only one parsed result of a successful cryptographic verifier is accepted."""
        selected = envelope()
        claim = object_value(object_value(selected["attestation"])["predicate"])
        outputs: tuple[JsonValue, ...] = ([], [{}], verified_output(claim) * 2)
        for output in outputs:
            with self.subTest(output=output), self.assertRaises((trust.TrustError, KeyError)):
                attest.verified_statement(output, selected, claim)

    def test_all_subjects_are_verified_and_cached_proofs_are_not_reused(self) -> None:
        """Every use verifies the actual archive, SBOM and runtime disposition bytes again."""
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            selected, claim = fixture(directory)
            output = json.dumps(verified_output(claim)).encode()
            calls: list[list[str]] = []

            def verified_command(
                argv: Sequence[str], **_options: object
            ) -> tuple[int, bytes, bytes]:
                calls.append(list(argv))
                return 0, output, b""

            with patch.object(bounded_process, "run", side_effect=verified_command) as run:
                first = attest.verify_directory(directory, selected)
                second = attest.verify_directory(directory, selected)
            self.assertEqual(run.call_count, 2 * len(trust.SUBJECTS))
            self.assertEqual(first, second)
            self.assertEqual(
                [Path(call[3]).name for call in calls],
                [*trust.SUBJECTS, *trust.SUBJECTS],
            )
            trust.bind_received(directory, first)

    def test_nonzero_verifier_cannot_authorize_a_success_looking_payload(self) -> None:
        """An invalid signature remains a failure even when stdout looks plausible."""
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            selected, claim = fixture(directory)
            with (
                patch.object(
                    bounded_process,
                    "run",
                    return_value=(1, json.dumps(verified_output(claim)).encode(), b""),
                ),
                self.assertRaisesRegex(trust.TrustError, "signature_failed"),
            ):
                _ = attest.verify_directory(directory, selected)

    def test_changed_bundle_or_bound_file_during_verification_is_refused(self) -> None:
        """Verification results cannot bless a later replacement of the inspected bytes."""
        for name in (
            trust.BUNDLE,
            "sbom.spdx.json",
            "source.json",
            "runtime-proof.json",
            "vex.openvex.json",
        ):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                selected, claim = fixture(directory)
                response_bytes = json.dumps(verified_output(claim)).encode()

                def mutate(
                    _argv: Sequence[str],
                    *,
                    changed: Path = directory / name,
                    response: bytes = response_bytes,
                    **_options: object,
                ) -> tuple[int, bytes, bytes]:
                    _ = changed.write_bytes(b"changed")
                    return 0, response, b""

                with (
                    patch.object(bounded_process, "run", side_effect=mutate),
                    self.assertRaises(ValueError),
                ):
                    _ = attest.verify_directory(directory, selected)

    def test_receiver_requires_proof_and_hashes_every_received_file(self) -> None:
        """An authenticated transport cannot accidentally publish an unbound extra candidate."""
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            selected, _ = fixture(directory)
            trust.bind_received(directory, selected)
            for name in trust.BOUND_FILES:
                original = (directory / name).read_bytes()
                _ = (directory / name).write_bytes(original + b" ")
                with self.subTest(name=name), self.assertRaises(trust.TrustError):
                    trust.bind_received(directory, selected)
                _ = (directory / name).write_bytes(original)
            del selected["attestation"]
            with self.assertRaises(receiver.FetchError):
                _ = receiver.validate_envelope(selected)

    def test_cached_directory_is_bound_to_the_exact_api_digested_zip(self) -> None:
        """Neither edited extraction nor a different ZIP can be reused as the selected artifact."""
        for name in ("artifact.zip", trust.PREDICATE, trust.BUNDLE, "source.json"):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                selected, _ = fixture(directory)
                _ = (directory / name).write_bytes(b"replacement")
                with patch.object(bounded_process, "run") as verifier:
                    with self.assertRaises(trust.TrustError):
                        _ = attest.verify_directory(directory, selected)
                    verifier.assert_not_called()


if __name__ == "__main__":
    _ = unittest.main()
