"""Prevent unchanged-input reuse from hiding changed fixtures, tools or executables."""

from __future__ import annotations

import ast
import os
import platform
import shlex
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
        self.assertFalse((ROOT / "results").is_symlink())
        (ROOT / "results").mkdir(mode=0o700, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="ci-verified space.", dir=ROOT / "results")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.git("init", "--quiet")
        for name in (
            "src/main.rs",
            "docs/example.md",
            "vendor/worker.cpp",
            "xtask/src/check.rs",
            "build/native_security.py",
            "security/exceptions.json",
            "security/native/toolchain.json",
            ".github/workflows/ci.yml",
            ".github/workflows/security.yml",
            "web/src/media.ts",
            "web/tests/media.test.mjs",
            "web/tests/layer-cap-cases.json",
            "web/e2e/package-lock.json",
        ):
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            _ = path.write_text("original\n")
        _ = (self.root / ".github/workflows/ci.yml").write_text(
            (ROOT / ".github/workflows/ci.yml").read_text()
        )
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

    def backend_fixture(self, dependencies: tuple[str, ...] = ("src/main.rs",)) -> Path:
        """Create both executables and Cargo-format dependency rules, including escaped spaces."""
        output = self.root / "target/debug"
        output.mkdir(parents=True, exist_ok=True)
        for name in cache.BINARIES:
            executable = output / name
            _ = executable.write_bytes(b"owned executable fixture")
            executable.chmod(0o700)
            words = [str(executable) + ":", *(str(self.root / path) for path in dependencies)]
            _ = (output / (name + ".d")).write_text(
                " \\\n  ".join(word.replace(" ", "\\ ") for word in words) + "\n\n"
            )
        return output

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

    def test_frontend_helper_tests_and_guides_reuse_existing_rust_receipts(self) -> None:
        """Independent frontend edits still execute their checks without rerunning Rust."""
        independent = (
            "build/tests/community-runner.test.mjs",
            "docs/performance.md",
            "web/e2e/community.cjs",
        )
        for name in independent:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            _ = path.write_text("original\n")
        self.git("add", ".")
        originals = {scope: cache.cache_key(self.root, scope) for scope in cache.SCOPES}
        for scope in ("rust-lint", "rust-test"):
            cache.save(self.root, scope, originals[scope], self.root / "cache" / scope)

        def assert_reused() -> None:
            for scope, original in originals.items():
                if scope == "automation":
                    self.assertNotEqual(cache.cache_key(self.root, scope), original, scope)
                else:
                    self.assertEqual(cache.cache_key(self.root, scope), original, scope)
            for scope in ("rust-lint", "rust-test"):
                self.assertEqual(
                    cache.verify(self.root, scope, originals[scope], self.root / "cache" / scope),
                    "1" * 40,
                )

        for name in (*independent, "web/src/media.ts"):
            _ = (self.root / name).write_text("changed independent input\n")
        assert_reused()
        self.git("update-index", "--chmod=+x", independent[0])
        assert_reused()
        additions = ("build/tests/new-helper.test.mjs", "docs/guides/new-guide.md")
        for name in additions:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            _ = path.write_text("new independent input\n")
        self.git("add", *additions)
        assert_reused()
        self.git("rm", "--force", "--", *independent[:2], *additions)
        assert_reused()

    def test_rust_receipts_keep_sources_shared_data_and_runtime_inputs_bound(self) -> None:
        """Similar filenames cannot hide a changed compiled or executed dependency."""
        names = (
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            ".cargo/config.toml",
            "src/main.rs",
            "vendor/worker.cpp",
            "vendor/README.md",
            "migrations/001_initial.sql",
            "security/authorization/operations.json",
            "web/tests/layer-cap-cases.json",
            "build/shutdown-smoke.mjs",
            "build/diagnostics-smoke.mjs",
            "xtask/src/test_env/server.rs",
            "build/tests/fixture-server.mjs",
            "build/tests/nested/helper.test.mjs",
            "build/tests/shared-fixture.json",
            "docs/shared-fixture.json",
            "docs/tool.rs",
            "README.md",
            "Caddyfile",
            "build/ci_verified.py",
            "build/security_tools.py",
            ".github/workflows/ci.yml",
            ".github/actions/native-toolchain/action.yml",
        )
        for name in names:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            if not path.exists():
                _ = path.write_text("original\n")
        self.git("add", ".")
        originals = {
            scope: cache.cache_key(self.root, scope) for scope in ("rust-lint", "rust-test")
        }
        for scope, original in originals.items():
            cache.save(self.root, scope, original, self.root / "cache" / scope)
        for name in names:
            path = self.root / name
            original = path.read_text()
            _ = path.write_text("changed actual input\n")
            for scope, key in originals.items():
                with self.subTest(name=name, scope=scope):
                    self.assertNotEqual(cache.cache_key(self.root, scope), key)
                    with self.assertRaisesRegex(ToolError, "ci_cache_inputs_changed"):
                        _ = cache.verify(self.root, scope, key, self.root / "cache" / scope)
            _ = path.write_text(original)

    def test_changed_source_policy_workflow_mode_and_deleted_inputs_invalidate(self) -> None:
        """Content and tracked executable modes both belong to every proof."""
        key = cache.cache_key(self.root, "native-asan")
        for name in (
            "vendor/worker.cpp",
            "security/native/toolchain.json",
            ".github/workflows/security.yml",
        ):
            path = self.root / name
            _ = path.write_text("changed\n")
            self.assertNotEqual(cache.cache_key(self.root, "native-asan"), key)
            _ = path.write_text("original\n")
        self.git("update-index", "--chmod=+x", "build/native_security.py")
        self.assertNotEqual(cache.cache_key(self.root, "native-asan"), key)
        (self.root / "vendor/worker.cpp").unlink()
        with self.assertRaisesRegex(ToolError, "ci_cache_source_missing"):
            _ = cache.cache_key(self.root, "native-asan")

    def test_native_keys_cover_all_real_build_inputs_without_unrelated_rust_or_docs(self) -> None:
        """Every actual native build input stays bound as unrelated application work changes."""
        for source in native_security.source_files(ROOT):
            self.assertTrue(cache.native_security_input(str(source.relative_to(ROOT))))
        originals = {
            scope: cache.cache_key(self.root, scope) for scope in ("native-asan", "codeql-native")
        }
        rust = cache.cache_key(self.root, "codeql-rust")
        for name in ("src/main.rs", "docs/example.md"):
            _ = (self.root / name).write_text("changed\n")
        for scope, key in originals.items():
            self.assertEqual(cache.cache_key(self.root, scope), key)
        self.assertNotEqual(cache.cache_key(self.root, "codeql-rust"), rust)

    def test_native_security_binds_each_helper_and_ignores_unrelated_review_changes(self) -> None:
        """Reviewing another analyzer must not rerun unchanged sanitizer and replay suites."""
        unrelated = (
            *cache.CODEQL_POLICY_DATA,
            ".github/workflows/ci.yml",
            ".github/workflows/codeql.yml",
            "security/authorization/operations.json",
            "security/image-policy.json",
            "build/security_codeql_local.py",
            "build/security_codeql_cache.py",
            "build/security_image.py",
            "ops/ansible/files/release_public.py",
        )
        native = (
            *cache.NATIVE_FILES,
            "vendor/integrity.json",
            "vendor/native-components.json",
            "security/native/corpus.json",
            "security/native/corpus/rtp/header-short.hex",
        )
        for name in (*native, *unrelated):
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            _ = path.write_text("original\n")
        self.git("add", ".")
        originals = {
            scope: cache.cache_key(self.root, scope)
            for scope in ("native-asan", "native-ubsan", "native-replay")
        }
        for name in unrelated:
            _ = (self.root / name).write_text("changed\n")
            for scope, original in originals.items():
                self.assertEqual(cache.cache_key(self.root, scope), original, name)
        for name in native:
            path = self.root / name
            _ = path.write_text("changed\n")
            for scope, original in originals.items():
                self.assertNotEqual(cache.cache_key(self.root, scope), original, name)
            _ = path.write_text("original\n")

    def test_native_security_closure_covers_all_local_python_imports(self) -> None:
        """New transitive imports cannot silently bypass native success invalidation."""
        modules = {
            path.stem: path
            for directory in (ROOT / "build", ROOT / "ops/ansible/files")
            for path in directory.glob("*.py")
        }
        pending = [
            "native_security",
            "native_security_cache",
            "security_vendor",
            "ci_verified",
            "ci_local_act",
        ]
        visited: set[str] = set()
        while pending:
            name = pending.pop()
            if name in visited:
                continue
            visited.add(name)
            path = modules[name]
            self.assertTrue(cache.native_security_input(str(path.relative_to(ROOT))), name)
            for node in ast.walk(ast.parse(path.read_text())):
                imports: list[str] = []
                if isinstance(node, ast.Import):
                    imports = [alias.name.split(".")[0] for alias in node.names]
                elif isinstance(node, ast.ImportFrom) and node.module:
                    imports = [node.module.split(".")[0]]
                pending.extend(imported for imported in imports if imported in modules)

    def test_codeql_review_data_changes_do_not_invalidate_evaluated_databases(self) -> None:
        """Finding reviews rerun against reports; they cannot alter extraction or queries."""
        for name in cache.CODEQL_POLICY_DATA:
            self.assertTrue((ROOT / name).is_file(), name)
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            _ = path.write_text("original\n")
        self.git("add", ".")
        originals = {scope: cache.cache_key(self.root, scope) for scope in cache.SCOPES}
        for name in cache.CODEQL_POLICY_DATA:
            path = self.root / name
            _ = path.write_text("new exact finding review\n")
            for scope, original in originals.items():
                if scope.startswith(("codeql-", "native-")) or scope == "backend":
                    self.assertEqual(cache.cache_key(self.root, scope), original, name)
                else:
                    self.assertNotEqual(cache.cache_key(self.root, scope), original, name)
            _ = path.write_text("original\n")

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
            "vendor/mediasoup-sys-0.19.0/tasks.py",
            "vendor/mediasoup-sys-0.19.0/meson.build",
            "vendor/mediasoup-sys-0.19.0/python-tools-requirements.txt",
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

    def test_native_codeql_closure_covers_all_local_python_imports(self) -> None:
        """Neither analysis can silently acquire an unkeyed transitive Python helper."""
        modules = {
            path.stem: path
            for directory in (ROOT / "build", ROOT / "ops/ansible/files")
            for path in directory.glob("*.py")
        }
        pending = ["security_codeql_local", "security_codeql_cache", "security_vendor"]
        visited: set[str] = set()
        while pending:
            name = pending.pop()
            if name in visited:
                continue
            visited.add(name)
            path = modules[name]
            self.assertTrue(cache.native_codeql_input(str(path.relative_to(ROOT))), name)
            self.assertTrue(cache.rust_codeql_input(str(path.relative_to(ROOT))), name)
            for node in ast.walk(ast.parse(path.read_text())):
                imports: list[str] = []
                if isinstance(node, ast.Import):
                    imports = [alias.name.split(".")[0] for alias in node.names]
                elif isinstance(node, ast.ImportFrom) and node.module:
                    imports = [node.module.split(".")[0]]
                pending.extend(imported for imported in imports if imported in modules)

    def test_rust_analysis_keeps_all_targets_embedded_data_and_tool_inputs(self) -> None:
        """New targets and shared fixtures invalidate Rust even outside its usual src directory."""
        inputs = (
            *cache.CODEQL_NATIVE_FILES,
            "Cargo.lock",
            "Cargo.toml",
            "rust-toolchain.toml",
            ".cargo/config.toml",
            "src/main.rs",
            "src/nested/fixture.txt",
            "load_tests/src/browser_profile.rs",
            "tests/authorization.rs",
            "new_target/module.rs",
            "web/new_target/helper.rs",
            "new_target/Cargo.toml",
            "new_target/rust-project.json",
            "migrations/001_example.sql",
            "vendor/mediasoup-sys-0.19.0/build.rs",
            "vendor/mediasoup-sys-0.19.0/meson.build",
            "vendor/seclists-passwords/10k-most-common.txt",
            "security/authorization/operations.json",
            "security/codeql/new-query.ql",
            "web/tests/layer-cap-cases.json",
        )
        for name in inputs:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            _ = path.write_text("original\n")
        self.git("add", ".")
        key = cache.cache_key(self.root, "codeql-rust")
        for name in inputs:
            with self.subTest(input=name):
                path = self.root / name
                _ = path.write_text("changed\n")
                self.assertNotEqual(cache.cache_key(self.root, "codeql-rust"), key)
                _ = path.write_text("original\n")
        self.git("update-index", "--chmod=+x", "new_target/module.rs")
        self.assertNotEqual(cache.cache_key(self.root, "codeql-rust"), key)

    def test_rust_analysis_ignores_unrelated_docs_deployment_and_frontend_tooling(self) -> None:
        """An unchanged Rust database survives independent documentation and operational work."""
        unrelated = (
            "README.md",
            "docs/testing.md",
            "ops/ansible/files/release_public.py",
            "ops/ansible/templates/public-app.env.j2",
            "build/security_image.py",
            "build/deploy.py",
            ".github/workflows/ci.yml",
            "security/image-policy.json",
            "Dockerfile",
            "web/e2e/package-lock.json",
            "web/package.json",
            "web/tests/participant-hovercard.test.mjs",
            "load_tests/benchmark-local.mjs",
        )
        for name in unrelated:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            _ = path.write_text("original\n")
        self.git("add", ".")
        key = cache.cache_key(self.root, "codeql-rust")
        for name in unrelated:
            _ = (self.root / name).write_text("changed\n")
            self.assertEqual(cache.cache_key(self.root, "codeql-rust"), key, name)

    def test_native_codeql_ignores_unrelated_ci_tools_but_binds_native_data_and_modes(self) -> None:
        """Application/deployment/image tooling does not alter standalone native extraction."""
        unrelated = (
            ".github/workflows/ci.yml",
            ".github/actions/native-toolchain/action.yml",
            "build/security_image.py",
            "build/ci_local_act.py",
            "ops/ansible/files/release_public.py",
            "security/image-policy.json",
            "Dockerfile",
        )
        native = (
            "vendor/native-components.json",
            "vendor/integrity.json",
            "vendor/mediasoup-sys-0.19.0/subprojects/libuv.wrap",
            "vendor/mediasoup-sys-0.19.0/subprojects/packagefiles/libuv/meson.build",
            "security/codeql/native-coverage/qlpack.yml",
            "build/codeql-native-build.sh",
        )
        for name in (*unrelated, *native):
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            _ = path.write_text("original\n")
        self.git("add", ".")
        original = cache.cache_key(self.root, "codeql-native")
        for name in unrelated:
            _ = (self.root / name).write_text("changed\n")
            self.assertEqual(cache.cache_key(self.root, "codeql-native"), original, name)
        for name in native:
            path = self.root / name
            _ = path.write_text("changed\n")
            self.assertNotEqual(cache.cache_key(self.root, "codeql-native"), original, name)
            _ = path.write_text("original\n")
        self.git("update-index", "--chmod=+x", native[-1])
        self.assertNotEqual(cache.cache_key(self.root, "codeql-native"), original)

    def test_runner_architecture_trust_and_hosted_image_generation_are_bound(self) -> None:
        """A local result cannot cross into hosted, another ISA, or trusted main."""
        originals = {
            scope: cache.cache_key(self.root, scope) for scope in ("rust-lint", "rust-test")
        }
        for updates in (
            {"LOCAL_CI_RUNNER_IMAGE": "docker.io/owned/runner@sha256:" + "b" * 64},
            {"GITHUB_EVENT_NAME": "pull_request"},
            {"GITHUB_REF": "refs/heads/feature"},
            {"ACT": "", "ImageOS": "ubuntu24", "ImageVersion": "20261003.1"},
        ):
            with patch.dict(os.environ, updates):
                for scope, key in originals.items():
                    self.assertNotEqual(cache.cache_key(self.root, scope), key)
        with patch.object(platform, "machine", return_value="other-architecture"):
            for scope, key in originals.items():
                self.assertNotEqual(cache.cache_key(self.root, scope), key)
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

    def test_rust_reference_requirements_invalidate_original_success(self) -> None:
        """The selected Python's reviewed lock is part of the Rust-test receipt closure."""
        path = self.root / "ops/ansible/requirements.txt"
        path.parent.mkdir(parents=True, exist_ok=True)
        _ = path.write_bytes((ROOT / "ops/ansible/requirements.txt").read_bytes())
        self.git("add", ".")
        original = cache.cache_key(self.root, "rust-test")
        directory = self.root / "cache"
        cache.save(self.root, "rust-test", original, directory)
        _ = path.write_bytes(path.read_bytes() + b"\n# changed reviewed input\n")
        self.assertNotEqual(cache.cache_key(self.root, "rust-test"), original)
        with self.assertRaisesRegex(ToolError, "ci_cache_inputs_changed"):
            _ = cache.verify(self.root, "rust-test", original, directory)

    def test_backend_cache_checks_every_binary_before_installing_any(self) -> None:
        """Corrupt or symlinked restored executables must never reach the browser server."""
        output = self.backend_fixture()
        directory = self.root / "cache"
        key = cache.cache_key(self.root, "backend")
        cache.save(self.root, "backend", key, directory)
        for name in cache.BINARIES:
            (output / name).unlink()
            (output / (name + ".d")).unlink()
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

    def test_embedded_ops_catalog_changes_every_compiled_tool_key_and_original_receipt(
        self,
    ) -> None:
        """Key the catalog without admitting adjacent Ops JSON into narrow scopes."""
        path = self.root / "ops/ops-build-inputs.lock.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        _ = path.write_bytes(b"original reviewed catalog\n")
        adjacent = path.with_name("unrelated-local-selection.json")
        _ = adjacent.write_bytes(b"original unrelated JSON\n")
        self.git("add", ".")
        original = {scope: cache.cache_key(self.root, scope) for scope in cache.SCOPES}
        directory = self.root / "catalog-receipt"
        cache.save(self.root, "rust-test", original["rust-test"], directory)
        self.assertEqual(
            cache.verify(self.root, "rust-test", original["rust-test"], directory), "1" * 40
        )
        _ = path.write_bytes(b"changed reviewed catalog\n")
        for scope in cache.SCOPES:
            self.assertNotEqual(cache.cache_key(self.root, scope), original[scope], scope)
        with self.assertRaisesRegex(ToolError, "ci_cache_inputs_changed"):
            _ = cache.verify(self.root, "rust-test", original["rust-test"], directory)
        _ = path.write_bytes(b"original reviewed catalog\n")
        _ = adjacent.write_bytes(b"changed unrelated JSON\n")
        for scope in cache.SCOPES:
            conservative = scope in {"automation", "rust-lint", "rust-test"}
            self.assertEqual(
                cache.cache_key(self.root, scope) != original[scope], conservative, scope
            )

    def test_backend_closure_preserves_build_inputs_without_operational_invalidations(self) -> None:
        """Documentation and Python tests cannot change compiler inputs or embedded Rust data."""
        inputs = (
            *cache.BACKEND_FILES,
            "Cargo.lock",
            "Cargo.toml",
            "rust-toolchain.toml",
            ".cargo/config.toml",
            "load_tests/src/main.rs",
            "load_tests/src/metrics/measurement.rs",
            "migrations/new.sql",
            "src/embedded.txt",
            "other-target/helper.rs",
            "web/new-target/helper.rs",
            "web/tests/layer-cap-cases.json",
        )
        unrelated = (
            "README.md",
            "docs/testing.md",
            "ops/ansible/tests/test_security_ci.py",
            "ops/ansible/files/release_public.py",
            "security/exceptions.json",
            "web/e2e/package-lock.json",
            "web/package.json",
            "Dockerfile",
        )
        for name in (*inputs, *unrelated):
            if name == ".github/workflows/ci.yml":
                continue
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            _ = path.write_text("original\n")
        self.git("add", ".")
        original = cache.cache_key(self.root, "backend")
        for name in unrelated:
            _ = (self.root / name).write_text("unrelated edit\n")
            self.assertEqual(cache.cache_key(self.root, "backend"), original, name)
        for name in set(inputs) - {".github/workflows/ci.yml"}:
            path = self.root / name
            _ = path.write_text("changed compiler input\n")
            self.assertNotEqual(cache.cache_key(self.root, "backend"), original, name)
            _ = path.write_text("original\n")

    def test_backend_workflow_projection_preserves_the_actual_build_contract(self) -> None:
        """Execute key construction against real workflow edits, including rejected YAML shapes."""
        path = self.root / ".github/workflows/ci.yml"
        original = path.read_text()
        key = cache.cache_key(self.root, "backend")
        for before, after, changed in (
            ("name: Build production container", "name: Build the production image", False),
            ("test:stress", "test:stress-extra", False),
            ("cargo build --locked --workspace --bins", "cargo build --locked --bins", True),
            ('OPENSSL_STATIC: "1"', 'OPENSSL_STATIC: "0"', True),
            ("target/openssl-4.1.0", "target/openssl-4.1.1", True),
            ("cache-key: rust-build", "cache-key: different-build", True),
        ):
            with self.subTest(before=before):
                self.assertIn(before, original)
                _ = path.write_text(original.replace(before, after))
                self.assertEqual(cache.cache_key(self.root, "backend") != key, changed)
        for altered in (
            original + "\nenv:\n  RUSTFLAGS: changed\n",
            original.replace("  browser:\n", "  browser: &shared\n"),
            original.replace("    env:\n", "    env: *shared\n"),
            original.replace("Save the exact backend success", "renamed boundary"),
            original.replace(
                "  web:\n", "    defaults:\n      run:\n        shell: custom\n\n  web:\n"
            ),
            original.replace("  web:\n", "    container: changed\n\n  web:\n"),
        ):
            _ = path.write_text(altered)
            with self.assertRaisesRegex(ToolError, "ci_backend_workflow"):
                _ = cache.cache_key(self.root, "backend")

    def test_backend_setup_defaults_stay_stable_and_compiler_overrides_invalidate(self) -> None:
        """Pinned setup's observed exports cannot break cold publication or weaken overrides."""
        with patch.dict(os.environ):
            _ = os.environ.pop("CARGO_HOME", None)
            _ = os.environ.pop("CARGO_INCREMENTAL", None)
            before = cache.cache_key(self.root, "backend")
            os.environ.update(
                {
                    "CARGO_HOME": str(Path.home() / ".cargo"),
                    "CARGO_INCREMENTAL": "0",
                    "CARGO_TERM_COLOR": "always",
                    "CACHE_ON_FAILURE": "false",
                }
            )
            self.assertEqual(cache.cache_key(self.root, "backend"), before)
            for name in (
                *(cache.BACKEND_ENVIRONMENT - {"CARGO_TARGET_DIR"}),
                "CARGO_PROFILE_DEV_OPT_LEVEL",
                "CARGO_BUILD_RUSTFLAGS",
            ):
                with patch.dict(os.environ, {name: "changed"}):
                    self.assertNotEqual(cache.cache_key(self.root, "backend"), before, name)
            with (
                patch.dict(os.environ, {"CARGO_TARGET_DIR": str(self.root / "other-output")}),
                self.assertRaisesRegex(ToolError, "ci_backend_target_directory"),
            ):
                _ = cache.cache_key(self.root, "backend")
            with patch.dict(os.environ, {"CARGO_INCREMENTAL": "1"}):
                self.assertNotEqual(cache.cache_key(self.root, "backend"), before)

    def test_backend_dependency_guard_rejects_unbound_sources_before_publication(self) -> None:
        """A successfully compiled embedded file must still be tracked and keyed."""
        for dependency in ("docs/example.md", "src/untracked.txt", "target/unbound.rs"):
            path = self.root / dependency
            path.parent.mkdir(parents=True, exist_ok=True)
            _ = path.write_text("unbound input\n")
            _ = self.backend_fixture(("src/main.rs", dependency))
            destination = self.root / "cache"
            with self.assertRaisesRegex(ToolError, "ci_backend_dep_unkeyed"):
                cache.save(self.root, "backend", cache.cache_key(self.root, "backend"), destination)
            self.assertFalse(destination.exists())

    def test_backend_dependency_guard_binds_generated_inputs_and_depinfo_bytes(self) -> None:
        """Generated files inherit pinned build inputs; restored dep-info is authenticated."""
        generated = "target/debug/build/mediasoup-sys-1234567890abcdef/out/fbs.rs"
        openssl = "target/openssl-4.1.0/lib/libssl.a"
        aws_lc = "target/aws-lc-5.11.0/lib/libcrypto-awslc.a"
        aws_bindings = "target/debug/build/aws-lc-sys-1234567890abcdef/out/bindings.rs"
        generated_inputs = (generated, openssl, aws_lc, aws_bindings)
        for relative in generated_inputs:
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            _ = path.write_text("generated fixture\n")
        output = self.backend_fixture(("src/main.rs", "vendor", *generated_inputs))
        with patch.dict(
            os.environ,
            {
                "OPENSSL_DIR": str(self.root / "target/openssl-4.1.0"),
                "AWS_LC_SYS_SYSTEM_DIR": str(self.root / "target/aws-lc-5.11.0"),
            },
        ):
            key = cache.cache_key(self.root, "backend")
            directory = self.root / "cache"
            cache.save(self.root, "backend", key, directory)
            for relative in generated_inputs:
                (self.root / relative).unlink()
            for name in cache.BINARIES:
                (output / name).unlink()
            self.assertEqual(cache.verify(self.root, "backend", key, directory), "1" * 40)
            for name in cache.BINARIES:
                (output / name).unlink()
            _ = (directory / "load_test.d").write_text("changed dependency rule\n")
            with self.assertRaisesRegex(ToolError, "ci_cache_binary_changed"):
                _ = cache.verify(self.root, "backend", key, directory)
            self.assertFalse(any((output / name).exists() for name in cache.BINARIES))

    def test_backend_dependency_guard_rejects_wrong_targets_and_unsupported_rules(self) -> None:
        """Unsupported dependency formats and external files fail instead of being ignored."""
        output = self.backend_fixture()
        path = output / "simplestChat.d"
        original = path.read_text()
        for malformed in (
            original.replace("/simplestChat:", "/another-binary:"),
            original.rstrip() + " /unbound/compiler/source.rs\n",
            original + "# env-dep:UNBOUND=value\n",
            original.replace("src/main.rs", "target/openssl-3.5.8/lib/libssl.a"),
            original.replace(
                "src/main.rs", "target/debug/build/aws-lc-sys-1234567890abcdef/out/unbound.rs"
            ),
        ):
            _ = path.write_text(malformed)
            with self.assertRaisesRegex(ToolError, "ci_backend_dep"):
                cache.backend_dependencies(self.root, output, "simplestChat", built=True)

    def test_codeql_cannot_use_a_success_stamp_instead_of_a_traced_database(self) -> None:
        """Only the extraction-cache helper may use the native CodeQL input identity."""
        for scope in ("codeql-native", "codeql-rust"):
            key = cache.cache_key(self.root, scope)
            for operation in (cache.save, cache.verify):
                with self.assertRaisesRegex(ToolError, "ci_cache_codeql_requires_database"):
                    _ = operation(self.root, scope, key, self.root / "cache")


class VerifiedWorkflowTests(unittest.TestCase):
    """Keep volatile audits/browser behavior fresh and publish only completed successes."""

    def test_selected_python_installs_the_reviewed_wheel_lock_before_fingerprinting(self) -> None:
        """Reference subprocesses use pinned PyYAML under the selected Unicode interpreter."""
        workflow = yaml_value(
            (ROOT / ".github/workflows/ci.yml").read_text(), scalars_as_strings=True
        )
        steps = objects(obj(workflow, "jobs", "rust-test"), "steps")
        python = next(
            step
            for step in steps
            if string(step.get("uses", "")).startswith("actions/setup-python@")
        )
        fingerprint = next(step for step in steps if step.get("id") == "verified-inputs")
        self.assertLess(steps.index(python), steps.index(fingerprint))
        self.assertNotIn("if", fingerprint)
        self.assertNotIn("continue-on-error", fingerprint)
        self.assertEqual(
            string(fingerprint, "run").splitlines(),
            [
                "python -m pip install --require-hashes --only-binary=:all: "
                + "-r ops/ansible/requirements.txt",
                'cargo run --locked -q -p xtask -- ci key --scope "$CI_CACHE_SCOPE" '
                + "--github-output",
            ],
        )
        requirements = (ROOT / "ops/ansible/requirements.txt").read_text().splitlines()
        self.assertIn("--require-hashes", requirements)
        self.assertIn("--only-binary=:all:", requirements)

    def test_rollover_browser_acquisition_has_ten_minutes_without_weakening_runtime(self) -> None:
        """Slow APT font downloads keep the same pinned install and required release check."""
        workflow = yaml_value(
            (ROOT / ".github/workflows/ci.yml").read_text(), scalars_as_strings=True
        )
        job = obj(workflow, "jobs", "deployment")
        steps = objects(job, "steps")
        install = next(
            step for step in steps if step.get("name") == "Install pinned rollover browser tooling"
        )
        release = next(
            step
            for step in steps
            if step.get("name") == "App-only release and rollback on a disposable host"
        )
        self.assertEqual(install["timeout-minutes"], "10")
        self.assertEqual(release["timeout-minutes"], "20")
        self.assertEqual(job["timeout-minutes"], "95")
        self.assertNotIn("if", install)
        self.assertNotIn("continue-on-error", install)
        self.assertNotIn("if", release)
        self.assertNotIn("continue-on-error", release)
        self.assertEqual(
            obj(install, "env"),
            {"PLAYWRIGHT_BROWSERS_PATH": "${{ runner.temp }}/rollover-playwright"},
        )
        commands = [
            line
            for line in string(install, "run").splitlines()
            if line and not line.lstrip().startswith("#")
        ]
        self.assertEqual(
            commands,
            [
                "npm --prefix web/e2e ci --ignore-scripts",
                "npm --prefix web/e2e exec -- playwright install --with-deps chromium",
            ],
        )
        self.assertLess(steps.index(install), steps.index(release))

    def test_rust_unicode_oracle_is_pinned_before_fingerprints_cache_and_builds(self) -> None:
        """The release calendar oracle cannot inherit a runner's older Unicode database."""
        jobs = obj(
            yaml_value((ROOT / ".github/workflows/ci.yml").read_text(), scalars_as_strings=True),
            "jobs",
        )
        for name in ("rust-test", "automation"):
            with self.subTest(job=name):
                steps = objects(obj(jobs, name), "steps")
                selected = [
                    step
                    for step in steps
                    if string(step.get("uses", "")).startswith("actions/setup-python@")
                ]
                self.assertEqual(len(selected), 1)
                python = selected[0]
                self.assertEqual(
                    string(python, "uses"),
                    "actions/setup-python@ece7cb06caefa5fff74198d8649806c4678c61a1",
                )
                self.assertEqual(string(obj(python, "with"), "python-version"), "3.14.8")
                self.assertNotIn("if", python)
                self.assertNotIn("continue-on-error", python)
                protected = [
                    next(step for step in steps if step.get("id") == "verified-inputs"),
                    next(step for step in steps if step.get("id") == "verified"),
                ]
                if name == "rust-test":
                    protected.extend(
                        [
                            next(
                                step
                                for step in steps
                                if step.get("uses") == "./.github/actions/native-toolchain"
                            ),
                            next(
                                step
                                for step in steps
                                if string(step.get("run", "")).startswith("cargo build ")
                            ),
                            next(
                                step
                                for step in steps
                                if string(step.get("run", "")).startswith("cargo test ")
                            ),
                        ]
                    )
                else:
                    protected.append(
                        next(
                            step
                            for step in steps
                            if 'python3 -m venv "${RUNNER_TEMP}/ansible-controller"'
                            in string(step.get("run", ""))
                        )
                    )
                for step in protected:
                    self.assertLess(steps.index(python), steps.index(step))

    def test_rust_native_consumers_override_the_python_pkg_config_export(self) -> None:
        """The selected Python environment must not redirect native OpenSSL discovery."""
        workflow = obj(
            yaml_value((ROOT / ".github/workflows/ci.yml").read_text(), scalars_as_strings=True)
        )
        job = obj(workflow, "jobs", "rust-test")
        steps = objects(job, "steps")
        python = next(
            step
            for step in steps
            if string(step.get("uses", "")).startswith("actions/setup-python@")
        )
        self.assertEqual(obj(python, "with").get("update-environment", "true"), "true")
        canonical = "${{ github.workspace }}/target/openssl-4.1.0/lib/pkgconfig"
        self.assertEqual(string(obj(workflow, "env"), "PKG_CONFIG_PATH"), canonical)

        # setup-python writes GITHUB_ENV after workflow/job env has been applied.
        exported = {**obj(workflow, "env"), **obj(job, "env")}
        exported["PKG_CONFIG_PATH"] = "/opt/hostedtoolcache/Python/3.14.8/x64/lib/pkgconfig"
        self.assertNotEqual(exported["PKG_CONFIG_PATH"], canonical)
        consumers = [
            step
            for step in steps
            if step.get("uses") == "./.github/actions/native-toolchain"
            or "cargo " in string(step.get("run", ""))
            or "./target/debug/load_test " in string(step.get("run", ""))
        ]
        self.assertEqual(len(consumers), 12)
        for step in consumers:
            with self.subTest(step=step.get("name", step.get("uses"))):
                self.assertLess(steps.index(python), steps.index(step))
                local = obj(step, "env")
                self.assertEqual(string(local, "PKG_CONFIG_PATH"), canonical)
                effective = {**exported, **local}
                self.assertEqual(effective["PKG_CONFIG_PATH"], canonical)

    def test_native_success_receipts_do_not_enable_automated_vendor_scans(self) -> None:
        """Optional native cache helpers remain available without any required scan job."""
        jobs = obj(
            yaml_value(
                (ROOT / ".github/workflows/security.yml").read_text(), scalars_as_strings=True
            ),
            "jobs",
        )
        self.assertNotIn("native-security", jobs)
        self.assertNotIn("security-deep", jobs)
        for helper in ("build/native_security.py", "build/native_security_cache.py"):
            self.assertTrue(cache.native_security_input(helper), helper)

    def test_rust_tests_prime_their_own_complete_graph_before_executing_every_test(self) -> None:
        """A binary-only cache must not prevent the test dependency graph being retained."""
        jobs = obj(
            yaml_value((ROOT / ".github/workflows/ci.yml").read_text(), scalars_as_strings=True),
            "jobs",
        )
        for name, key in (("rust-test", "rust-test"), ("browser", "rust-build")):
            steps = objects(obj(jobs, name), "steps")
            toolchain = next(
                step for step in steps if step.get("uses") == "./.github/actions/native-toolchain"
            )
            self.assertEqual(string(obj(toolchain, "with"), "cache-key"), key)
            build = next(
                step for step in steps if string(step.get("run", "")).startswith("cargo build ")
            )
            arguments = shlex.split(string(build, "run"))
            self.assertIn("--locked", arguments)
            self.assertIn("--workspace", arguments)
            if name == "browser":
                self.assertIn("--bins", arguments)
                self.assertNotIn("--all-targets", arguments)
                continue
            self.assertIn("--all-targets", arguments)
            self.assertIn("--all-features", arguments)
            self.assertIn("--profile", arguments)
            self.assertEqual(arguments[arguments.index("--profile") + 1], "test")
            execute = next(
                step for step in steps if string(step.get("run", "")).startswith("cargo test ")
            )
            self.assertEqual(
                shlex.split(string(execute, "run")),
                [
                    "cargo",
                    "test",
                    "--locked",
                    "--workspace",
                    "--all-features",
                    "--",
                    "--include-ignored",
                    "--test-threads=1",
                    "--skip",
                    "capacity_vps::process::tests::inert_child_signal_mask",
                    "--skip",
                    "capacity_vps::host_tests::whole_host_policy_matches_original_python_oracle",
                    "--skip",
                    (
                        "ci::ops_artifact::capacity_vps::controller_tests::"
                        "all_eight_whole_reference_scenarios_preserve_local_policy_"
                        "and_explicit_transport_deltas"
                    ),
                    "--skip",
                    "release::build::tests::whole_reference_scenarios_match_unchanged_python",
                    "--skip",
                    "release::build::process::tests::signal_worker",
                    "--skip",
                    (
                        "release::attestation::prepare::oracle_tests::"
                        "whole_prepare_scenarios_match_unchanged_python"
                    ),
                    "--skip",
                    "release::attestation::prepare::signal_tests::retained_hash_signal_child",
                    "--skip",
                    "whole_verify_scenarios_match_unchanged_python",
                    "--skip",
                    "whole_fresh_release_matches_unchanged_python",
                ],
            )
            self.assertLess(steps.index(build), steps.index(execute))
            self.assertEqual(build["if"], execute["if"])

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
            if name == "Check helper syntax, safety and cleanup":
                self.assertEqual(step["run"], "cargo run --locked -q -p xtask -- check --helpers")
            else:
                self.assertEqual(step["run"], "cargo run --locked -q -p xtask -- ci audit-rust")
        self.assertEqual(obj(jobs, "rust-lint")["timeout-minutes"], "30")
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
                    step for step in steps if "xtask -- ci verify" in string(step.get("run", ""))
                )
                save = next(
                    step for step in steps if "xtask -- ci save" in string(step.get("run", ""))
                )
                self.assertEqual(verify["if"], "steps.verified.outputs.cache-hit == 'true'")
                self.assertEqual(save["if"], "steps.verified.outputs.cache-hit != 'true'")
                self.assertNotIn("always()", string(save, "if"))
                for step in steps:
                    if step.get("id") == "verified":
                        self.assertNotIn("restore-keys", obj(step, "with"))
                    if "xtask -- ci " in string(step.get("run", "")):
                        self.assertNotIn("${{", string(step, "run"))


if __name__ == "__main__":
    _ = unittest.main()
