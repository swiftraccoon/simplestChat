"""Prevent unchanged-input reuse from hiding changed fixtures, tools or executables."""

from __future__ import annotations

import os
import platform
import subprocess
import tempfile
import unittest
from pathlib import Path
from typing import override
from unittest.mock import patch

from test_support import ROOT, obj, objects, string, yaml_value

# isort: split
import ci_verified as cache
import native_security
from security_tools import ToolError


class VerifiedCacheTests(unittest.TestCase):
    """Exercise actual tracked files and receipts without a runner or remote cache."""

    def __init__(self, method_name: str = "runTest") -> None:
        """Initialize typed paths for the disposable checkout."""
        super().__init__(method_name)
        self.root: Path = ROOT

    @override
    def setUp(self) -> None:
        """Create an owned Git index containing representative coupled inputs."""
        temporary = tempfile.TemporaryDirectory(prefix="ci-verified.", dir=ROOT / "results")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.git("init", "--quiet")
        for name in (
            "src/main.rs",
            "docs/example.md",
            "vendor/worker.cpp",
            "build/check.sh",
            "security/exceptions.json",
            ".github/workflows/ci.yml",
            "web/src/media.ts",
            "web/tests/media.test.mjs",
            "web/tests/layer-cap-cases.json",
            "web/e2e/package-lock.json",
        ):
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            _ = path.write_text("original\n")
        self.git("add", ".")
        self.addCleanup(os.environ.update, dict(os.environ))
        self.addCleanup(os.environ.clear)
        os.environ.update(
            {
                "ACT": "true",
                "LOCAL_CI_RUNNER_IMAGE": "docker.io/owned/runner@sha256:" + "a" * 64,
                "GITHUB_REF": "refs/heads/main",
                "GITHUB_EVENT_NAME": "push",
                "GITHUB_SHA": "1" * 40,
            },
        )

    def git(self, *arguments: str) -> None:
        """Mutate only this test's index; no commit or remote is created."""
        _ = subprocess.run(  # noqa: S603 -- Fixed test-owned Git arguments.
            ["git", *arguments],  # noqa: S607 -- Match the maintained helper's Git prerequisite.
            cwd=self.root,
            check=True,
            capture_output=True,
            timeout=10,
        )

    def test_frontend_code_reuses_backend_but_shared_json_fixture_invalidates_it(self) -> None:
        """Rust includes web/tests/layer-cap-cases.json at compile time."""
        original = {scope: cache.cache_key(self.root, scope) for scope in cache.SCOPES}
        _ = (self.root / "web/src/media.ts").write_text("new camera implementation\n")
        _ = (self.root / "web/tests/media.test.mjs").write_text("new frontend regression\n")
        for scope in cache.SCOPES:
            with self.subTest(scope=scope):
                current = cache.cache_key(self.root, scope)
                if scope == "automation":
                    self.assertNotEqual(current, original[scope])
                else:
                    self.assertEqual(current, original[scope])
        _ = (self.root / "web/tests/layer-cap-cases.json").write_text("changed shared cases\n")
        for scope in ("rust-test", "rust-lint", "backend", "codeql-rust"):
            self.assertNotEqual(cache.cache_key(self.root, scope), original[scope])

    def test_changed_source_policy_workflow_mode_and_deleted_inputs_invalidate(self) -> None:
        """Content and tracked executable modes both belong to every proof."""
        key = cache.cache_key(self.root, "native-asan")
        for name in ("vendor/worker.cpp", "security/exceptions.json", ".github/workflows/ci.yml"):
            path = self.root / name
            _ = path.write_text("changed\n")
            self.assertNotEqual(cache.cache_key(self.root, "native-asan"), key)
            _ = path.write_text("original\n")
        self.git("update-index", "--chmod=+x", "build/check.sh")
        self.assertNotEqual(cache.cache_key(self.root, "native-asan"), key)
        (self.root / "vendor/worker.cpp").unlink()
        with self.assertRaisesRegex(ToolError, "ci_cache_source_missing"):
            _ = cache.cache_key(self.root, "native-asan")

    def test_native_keys_cover_all_real_build_inputs_without_unrelated_rust_or_docs(self) -> None:
        """Every actual native build input stays bound as unrelated application work changes."""
        for source in native_security.source_files(ROOT):
            self.assertTrue(str(source.relative_to(ROOT)).startswith(cache.NATIVE_PREFIXES))
        originals = {
            scope: cache.cache_key(self.root, scope) for scope in ("native-asan", "codeql-native")
        }
        rust = cache.cache_key(self.root, "codeql-rust")
        for name in ("src/main.rs", "docs/example.md"):
            _ = (self.root / name).write_text("changed\n")
        for scope, key in originals.items():
            self.assertEqual(cache.cache_key(self.root, scope), key)
        self.assertNotEqual(cache.cache_key(self.root, "codeql-rust"), rust)

    def test_native_codeql_binds_its_actual_build_analysis_and_policy_inputs(self) -> None:
        """Traced worker setup, query identity and runtime helpers must still invalidate reuse."""
        names = (
            "build/codeql-native-build.sh",
            "build/pip-constraints.txt",
            "build/install-openssl.sh",
            "build/security_codeql_resources.py",
            "build/security_codeql_local.py",
            "build/security_codeql_cache.py",
            "build/security_codeql_tools.py",
            "build/security_codeql_triage.py",
            "build/security_vendor.py",
            "security/codeql-toolchain.json",
            "security/codeql/native-coverage/compilations.ql",
            ".github/workflows/codeql.yml",
            "ops/ansible/files/bounded_process.py",
            "vendor/mediasoup-sys-0.17.0/tasks.py",
            "vendor/mediasoup-sys-0.17.0/meson.build",
            "vendor/mediasoup-sys-0.17.0/python-tools-requirements.txt",
        )
        for name in names:
            self.assertTrue((ROOT / name).is_file())
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            _ = path.write_text("original\n")
        self.git("add", ".")
        original = cache.cache_key(self.root, "codeql-native")
        for name in names:
            with self.subTest(input=name):
                path = self.root / name
                _ = path.write_text("changed\n")
                self.assertNotEqual(cache.cache_key(self.root, "codeql-native"), original)
                _ = path.write_text("original\n")

    def test_runner_architecture_trust_and_hosted_image_generation_are_bound(self) -> None:
        """A local result cannot cross into hosted, another ISA, or trusted main."""
        key = cache.cache_key(self.root, "rust-test")
        for updates in (
            {"LOCAL_CI_RUNNER_IMAGE": "docker.io/owned/runner@sha256:" + "b" * 64},
            {"GITHUB_EVENT_NAME": "pull_request"},
            {"GITHUB_REF": "refs/heads/feature"},
            {"ACT": "", "ImageOS": "ubuntu24", "ImageVersion": "20261003.1"},
        ):
            with patch.dict(os.environ, updates):
                self.assertNotEqual(cache.cache_key(self.root, "rust-test"), key)
        with patch.object(platform, "machine", return_value="other-architecture"):
            self.assertNotEqual(cache.cache_key(self.root, "rust-test"), key)
        with (
            patch.dict(os.environ, {"LOCAL_CI_RUNNER_IMAGE": "owned/runner:latest"}),
            self.assertRaisesRegex(ToolError, "ci_cache_local_image"),
        ):
            _ = cache.cache_key(self.root, "rust-test")

    def test_receipt_reuse_names_original_revision_and_refuses_stale_inputs(self) -> None:
        """Reuse is explicit prior proof, not a claim of reexecution at the new revision."""
        directory = self.root / "cache"
        key = cache.cache_key(self.root, "rust-test")
        cache.save(self.root, "rust-test", key, directory)
        with patch.dict(os.environ, {"GITHUB_SHA": "2" * 40}):
            self.assertEqual(cache.verify(self.root, "rust-test", key, directory), "1" * 40)
        _ = (self.root / "src/main.rs").write_text("different backend\n")
        with self.assertRaisesRegex(ToolError, "ci_cache_inputs_changed"):
            _ = cache.verify(self.root, "rust-test", key, directory)
        with self.assertRaisesRegex(ToolError, "ci_cache_inputs_changed"):
            cache.save(self.root, "rust-test", key, self.root / "changed-cache")
        self.assertFalse((self.root / "changed-cache").exists())

    def test_backend_cache_checks_every_binary_before_installing_any(self) -> None:
        """Corrupt or symlinked restored executables must never reach the browser server."""
        output = self.root / "target/debug"
        output.mkdir(parents=True)
        for name in cache.BINARIES:
            path = output / name
            _ = path.write_bytes(b"owned executable fixture")
            path.chmod(0o700)
        directory = self.root / "cache"
        key = cache.cache_key(self.root, "backend")
        cache.save(self.root, "backend", key, directory)
        for name in cache.BINARIES:
            (output / name).unlink()
        self.assertEqual(cache.verify(self.root, "backend", key, directory), "1" * 40)
        for name in cache.BINARIES:
            self.assertEqual((output / name).read_bytes(), b"owned executable fixture")
            (output / name).unlink()
        _ = (directory / "load_test").write_bytes(b"modified executable")
        with self.assertRaisesRegex(ToolError, "ci_cache_binary_changed"):
            _ = cache.verify(self.root, "backend", key, directory)
        self.assertEqual(list(output.iterdir()), [])
        (directory / "load_test").unlink()
        (directory / "load_test").symlink_to(directory / "simplestChat")
        with self.assertRaises(OSError):
            _ = cache.verify(self.root, "backend", key, directory)

    def test_codeql_cannot_use_a_success_stamp_instead_of_a_traced_database(self) -> None:
        """Only the extraction-cache helper may use the native CodeQL input identity."""
        for scope in ("codeql-native", "codeql-rust"):
            key = cache.cache_key(self.root, scope)
            for operation in (cache.save, cache.verify):
                with self.assertRaisesRegex(ToolError, "ci_cache_codeql_requires_database"):
                    _ = operation(self.root, scope, key, self.root / "cache")


class VerifiedWorkflowTests(unittest.TestCase):
    """Keep volatile audits/browser behavior fresh and publish only completed successes."""

    def test_audits_helpers_and_browser_suites_are_not_replaced_by_cached_success(self) -> None:
        """Cached Rust work cannot suppress UI or advisory checks."""
        jobs = obj(
            yaml_value((ROOT / ".github/workflows/ci.yml").read_text(), scalars_as_strings=True),
            "jobs",
        )
        lint = objects(obj(jobs, "rust-lint"), "steps")
        for name in ("Audit Rust dependencies", "Check helper syntax, safety and cleanup"):
            step = next(step for step in lint if step.get("name") == name)
            self.assertNotIn("if", step)
        browser = objects(obj(jobs, "browser"), "steps")
        for step in browser:
            if "node web/e2e/" in string(step.get("run", "")):
                self.assertNotIn("verified", string(step.get("if", "")))
        toolchain = next(
            step for step in browser if step.get("uses") == "./.github/actions/native-toolchain"
        )
        self.assertEqual(toolchain["if"], "steps.verified.outputs.cache-hit != 'true'")

    def test_cache_receipts_are_verified_and_saved_only_on_success_without_prefix_fallback(
        self,
    ) -> None:
        """Every reusable success is exact and cannot hide a failed original command."""
        for filename in ("ci.yml", "security.yml"):
            jobs = obj(
                yaml_value(
                    (ROOT / ".github/workflows" / filename).read_text(), scalars_as_strings=True
                ),
                "jobs",
            )
            for raw in jobs.values():
                steps = objects(obj(raw).get("steps", []))
                restores = [step for step in steps if step.get("id") == "verified-inputs"]
                if not restores:
                    continue
                verify = next(
                    step for step in steps if "ci_verified.py verify" in string(step.get("run", ""))
                )
                save = next(
                    step for step in steps if "ci_verified.py save" in string(step.get("run", ""))
                )
                self.assertEqual(verify["if"], "steps.verified.outputs.cache-hit == 'true'")
                self.assertEqual(save["if"], "steps.verified.outputs.cache-hit != 'true'")
                self.assertNotIn("always()", string(save, "if"))
                for step in steps:
                    if step.get("id") == "verified":
                        self.assertNotIn("restore-keys", obj(step, "with"))
                    if "ci_verified.py" in string(step.get("run", "")):
                        self.assertNotIn("${{", string(step, "run"))


if __name__ == "__main__":
    _ = unittest.main()
