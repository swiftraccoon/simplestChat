"""Exact image fixtures exercise conditional VEX without running image code."""

from __future__ import annotations

import copy
import json
import re
import shlex
import shutil
import stat
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from test_support import ROOT, obj

# isort: split
import security_elf
import security_image as image
import security_image_policy as policy
import security_runtime as runtime
from release_json import JsonObject, JsonValue, array_value, decode_json, object_value
from security_tools import ToolError
from test_security_elf import rootfs
from test_security_image_advisories import empty_grype, openssl
from test_security_image_evidence import native_inventory

REVISION = "b" * 40
ARCHIVE = "c" * 64


class Fixture:
    """Create a bounded fake image with real metadata-only ELF and native receipt schemas."""

    def __init__(self, base: Path) -> None:
        """Keep checkout policy and image evidence in separate owned directories."""
        self.root: Path = base / "checkout"
        self.tree: Path = base / "output/archive"
        for name in (*runtime.SOURCE_FILES, runtime.PROFILE, runtime.NSS, runtime.REVIEW):
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            _ = path.write_bytes((ROOT / name).read_bytes())
        (self.tree / "layers").mkdir(parents=True)
        (self.tree / "rootfs").mkdir()
        rootfs(self.tree / "rootfs")
        (self.tree / "rootfs/etc").mkdir()
        _ = (self.tree / "rootfs/etc/nsswitch.conf").write_bytes((ROOT / runtime.NSS).read_bytes())
        self.elf: JsonObject = object_value(
            decode_json(json.dumps(security_elf.audit(self.tree / "rootfs")))
        )
        self.native: JsonObject = native_inventory()
        self.native["binary"] = {
            "sha256": self.elf["binarySha256"],
            "size": self.elf["binaryBytes"],
        }
        image.write(self.tree.parent / "native.json", self.native)
        image.write(self.tree.parent / "elf.json", self.elf)
        self.manifest: JsonObject = {
            "revision": REVISION,
            "archiveSha256": ARCHIVE,
            "platform": "linux/amd64",
        }
        self.config: JsonObject = {
            "os": "linux",
            "architecture": "amd64",
            "config": {
                "User": "10001:10001",
                "WorkingDir": "/app",
                "Cmd": ["/app/simplestChat"],
                "Env": ["PATH=/usr/bin"],
                "Labels": {"org.opencontainers.image.revision": REVISION},
            },
        }
        self.config_path: Path = self.tree / "layers/image-config.json"
        self.refresh_config()

    def refresh_config(self) -> None:
        """Retain agreeing archive/config identity after an intentional fixture mutation."""
        body = json.dumps(self.config).encode()
        _ = self.config_path.write_bytes(body)
        _ = (self.tree / "report.json").write_text(
            json.dumps(
                {
                    **self.manifest,
                    "passed": True,
                    "configSha256": runtime.sha256(body),
                    "imageId": "sha256:" + runtime.sha256(body),
                }
            )
        )

    def proof(self) -> JsonObject:
        """Exercise real artifact/native/profile checks with source identities tested separately."""
        with patch.object(
            runtime, "source_review", return_value={"sha256": "d" * 64, "expires": "2026-11-29"}
        ):
            return runtime.proof(
                tree=self.tree,
                elf=self.elf,
                native=self.native,
                manifest=self.manifest,
                root=self.root,
                sbom_sha256="e" * 64,
            )


class RuntimeSecurityTests(unittest.TestCase):
    """Missing, changed and expired evidence never becomes an artifact exception."""

    def test_runtime_recipe_replaces_nss_symlink_before_copy(self) -> None:
        """Actual cleanup operands and ordering produce the direct file required by proof."""
        recipe = (ROOT / "Dockerfile").read_text().split("AS runtime-base\n", 1)[1]
        stage = recipe.split("\nFROM ", 1)[0]
        cleanup = re.search(r"&& rm -f ([^\n]+)\n", stage)
        self.assertIsNotNone(cleanup)
        if cleanup is None:
            self.fail("Runtime cleanup instruction is missing")
        operands = shlex.split(cleanup[1])
        self.assertEqual(set(operands), {"/etc/ld.so.cache", "/etc/nsswitch.conf"})
        self.assertLess(
            cleanup.start(), stage.index("COPY security/runtime/nsswitch.conf /etc/nsswitch.conf")
        )
        expected = (ROOT / runtime.NSS).read_bytes()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "etc/authselect").mkdir(parents=True)
            target = root / "etc/authselect/nsswitch.conf"
            original = b"hosts: files systemd dns\n"
            _ = target.write_bytes(original)
            direct = root / "etc/nsswitch.conf"
            direct.symlink_to("authselect/nsswitch.conf")
            # Copying alone follows the base symlink and fails the unchanged proof.
            _ = shutil.copyfile(ROOT / runtime.NSS, direct)
            self.assertEqual(target.read_bytes(), expected)
            with self.assertRaisesRegex(ToolError, "runtime_nss_kind"):
                _ = runtime.filesystem_profile(root, expected)
            _ = target.write_bytes(original)
            _ = (root / "etc/ld.so.cache").write_bytes(b"old cache")
            # Apply only the checked recipe's two operands under this owned root.
            for operand in operands:
                (root / operand.removeprefix("/")).unlink()
            _ = shutil.copyfile(ROOT / runtime.NSS, direct)
            self.assertTrue(stat.S_ISREG(direct.lstat().st_mode))
            self.assertEqual(direct.read_bytes(), expected)
            self.assertEqual(target.read_bytes(), original)
            self.assertEqual(
                runtime.filesystem_profile(root, expected)["nssSha256"], runtime.sha256(expected)
            )

    def test_integrated_disposition_publishes_matching_proof_and_vex_hashes(self) -> None:
        """The pipeline's persisted artifacts agree with every public outcome binding."""
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Fixture(Path(temporary))
            output = fixture.tree.parent
            (output / "spdx").mkdir()
            _ = (output / "spdx/sbom.spdx.json").write_text("{}")
            outcome: JsonObject = {
                "imageId": "sha256:" + runtime.sha256(fixture.config_path.read_bytes())
            }
            with (
                patch.object(image, "ROOT", fixture.root),
                patch.object(
                    runtime,
                    "source_review",
                    return_value={"sha256": "d" * 64, "expires": "2026-11-29"},
                ),
            ):
                result = image.runtime_disposition(
                    tree=fixture.tree,
                    elf=fixture.elf,
                    native=fixture.native,
                    manifest=fixture.manifest,
                    vulnerabilities=policy.vulnerability_verdict(empty_grype(), [], [openssl()]),
                    output=output,
                    outcome=outcome,
                )
            self.assertTrue(result["passed"])
            self.assertTrue(outcome["vexRequired"])
            self.assertEqual(
                outcome["runtimeProofSha256"], image.digest(output / "runtime-proof.json")
            )
            self.assertEqual(outcome["vexSha256"], image.digest(output / "vex.openvex.json"))
            self.assertEqual(
                obj(result, "runtimeDisposition")["proofSha256"], outcome["runtimeProofSha256"]
            )

    def test_proof_and_vex_bind_published_bytes_and_keep_affected_findings(self) -> None:
        """Exact conditional success leaves the vulnerable RPM and original verdict visible."""
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Fixture(Path(temporary))
            proof = fixture.proof()
            path = fixture.tree.parent / "runtime-proof.json"
            image.write(path, proof)
            original = policy.vulnerability_verdict(empty_grype(), [], [openssl()])
            result, vex = runtime.apply(original, proof, path)
            self.assertTrue(result["passed"])
            self.assertFalse(original["passed"])
            self.assertEqual(result["affectedFindings"], original["blocked"])
            self.assertFalse(obj(result, "reviewedAdvisories")["passed"])
            self.assertEqual(obj(result, "runtimeDisposition")["proofSha256"], image.digest(path))
            self.assertEqual(
                proof["nativeSha256"], image.digest(fixture.tree.parent / "native.json")
            )
            self.assertEqual(proof["elfSha256"], image.digest(fixture.tree.parent / "elf.json"))
            product = obj(vex, "statements", 0, "products", 0)
            self.assertEqual(obj(product, "hashes")["sha-256"], ARCHIVE)
            self.assertEqual(obj(product, "subcomponents", 0)["@id"], openssl()["purl"])

    def test_unrelated_cve_or_package_stays_blocked(self) -> None:
        """The single advisory disposition cannot suppress another product or vulnerability."""
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Fixture(Path(temporary))
            proof = fixture.proof()
            path = fixture.tree.parent / "runtime-proof.json"
            image.write(path, proof)
            original = policy.vulnerability_verdict(empty_grype(), [], [openssl()])
            unrelated: list[JsonValue] = [
                {"id": "CVE-2026-1", "scope": openssl()["purl"]},
                {"id": runtime.CVE, "scope": "pkg:cargo/other@1"},
            ]
            original["blocked"] = [*unrelated, *array_value(original["blocked"])]
            result, _ = runtime.apply(original, proof, path)
            self.assertFalse(result["passed"])
            self.assertEqual(result["blocked"], unrelated)

    def test_config_overrides_and_artifact_identity_changes_fail(self) -> None:
        """An otherwise authenticated image still requires the exact execution profile."""
        overrides: list[tuple[str, JsonValue]] = [
            ("Env", ["LD_PRELOAD="]),
            ("Cmd", ["/bin/sh"]),
            ("Healthcheck", {"Test": ["CMD", "true"]}),
            ("User", "0"),
        ]
        for key, value in overrides:
            with self.subTest(key=key), tempfile.TemporaryDirectory() as temporary:
                fixture = Fixture(Path(temporary))
                obj(fixture.config, "config")[key] = value
                fixture.refresh_config()
                with self.assertRaisesRegex(ValueError, "runtime_"):
                    _ = fixture.proof()
        for key in ("archiveSha256", "revision", "platform"):
            with self.subTest(identity=key), tempfile.TemporaryDirectory() as temporary:
                fixture = Fixture(Path(temporary))
                fixture.manifest[key] = "changed"
                with self.assertRaisesRegex(ToolError, "runtime_archive_binding"):
                    _ = fixture.proof()

    def test_cache_preload_hwcaps_and_changed_nss_fail(self) -> None:
        """An empty hook or dangling symlink is still outside the reviewed profile."""
        for name in ("etc/ld.so.cache", "etc/ld.so.preload", "usr/lib64/glibc-hwcaps"):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temporary:
                fixture = Fixture(Path(temporary))
                (fixture.tree / "rootfs" / name).symlink_to("missing")
                with self.assertRaisesRegex(ToolError, "runtime_loader_hook|runtime_hwcaps"):
                    _ = fixture.proof()
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Fixture(Path(temporary))
            _ = (fixture.tree / "rootfs/etc/nsswitch.conf").write_text("hosts: systemd dns\n")
            with self.assertRaisesRegex(ToolError, "runtime_nss_profile"):
                _ = fixture.proof()

    def test_missing_native_settings_or_changed_evidence_fail(self) -> None:
        """Version text cannot replace configured static-archive and final byte evidence."""
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Fixture(Path(temporary))
            obj(fixture.native, "openssl", "build")["disabled_options"] = ["shared"]
            with self.assertRaisesRegex(ToolError, "runtime_static_openssl"):
                _ = fixture.proof()
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Fixture(Path(temporary))
            _ = (fixture.tree.parent / "elf.json").write_text("{}")
            with self.assertRaisesRegex(ToolError, "runtime_report_changed"):
                _ = fixture.proof()

    def test_no_affected_rpm_emits_no_vex_statement(self) -> None:
        """A fixed RPM needs no source exception and does not renew the review."""
        verdict = policy.vulnerability_verdict(empty_grype(), [], [openssl("3.5.9")])
        proof, vex = runtime.not_required(
            {"revision": REVISION, "archiveSha256": ARCHIVE, "platform": "linux/amd64"},
            "sha256:" + "a" * 64,
            verdict,
        )
        self.assertFalse(proof["required"])
        self.assertEqual(vex["statements"], [])
        with self.assertRaisesRegex(ToolError, "runtime_affected_rpm_requires_proof"):
            _ = runtime.not_required(
                {}, "", policy.vulnerability_verdict(empty_grype(), [], [openssl()])
            )

    def test_expired_changed_source_and_changed_profile_reviews_fail(self) -> None:
        """Review records are fixed inputs, never derived or refreshed during the gate."""
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Fixture(Path(temporary))
            review = object_value(decode_json((fixture.root / runtime.REVIEW).read_text()))

            def git_bytes(_root: Path, arguments: list[str], _limit: int) -> bytes:
                if arguments[0] == "rev-parse":
                    return (
                        "\n".join(str(obj(review, "trees")[name]) for name in runtime.SOURCE_TREES)
                        + "\n"
                    ).encode()
                return (fixture.root / arguments[1].split(":", 1)[1]).read_bytes()

            with patch.object(runtime, "git_bytes", side_effect=git_bytes):
                self.assertEqual(
                    runtime.source_review(fixture.root, REVISION)["expires"], "2026-11-29"
                )
                original = copy.deepcopy(review)
                review["expires"] = "2026-09-30"
                _ = (fixture.root / runtime.REVIEW).write_text(json.dumps(review))
                with self.assertRaisesRegex(ToolError, "runtime_source_review_expired"):
                    _ = runtime.source_review(fixture.root, REVISION)
                review = original
                _ = (fixture.root / runtime.REVIEW).write_text(json.dumps(review))
                _ = (fixture.root / runtime.PROFILE).write_text("changed")
                with self.assertRaisesRegex(ToolError, "runtime_source_file_changed"):
                    _ = runtime.source_review(fixture.root, REVISION)
            with (
                patch.object(runtime, "git_bytes", return_value=b"changed\n"),
                self.assertRaisesRegex(ToolError, "runtime_source_tree_changed"),
            ):
                _ = runtime.source_review(fixture.root, REVISION)

    def test_failed_proof_retains_original_affected_evidence(self) -> None:
        """A denied disposition cannot erase the pre-disposition scanner/advisory result."""
        verdict = policy.vulnerability_verdict(empty_grype(), [], [openssl()])
        outcome: JsonObject = {"imageId": "sha256:" + "a" * 64}
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            (output / "spdx").mkdir()
            _ = (output / "spdx/sbom.spdx.json").write_text("{}")
            with (
                patch.object(
                    runtime, "proof", side_effect=ToolError("runtime_source_review_expired")
                ),
                self.assertRaisesRegex(ToolError, "runtime_source_review_expired"),
            ):
                _ = image.runtime_disposition(
                    tree=output,
                    elf={},
                    native={},
                    manifest={},
                    vulnerabilities=verdict,
                    output=output,
                    outcome=outcome,
                )
        self.assertEqual(outcome["vulnerabilityFindings"], verdict)
