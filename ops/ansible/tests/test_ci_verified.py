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
            "build/check.sh",
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
            "load_tests/clients/browser_profile.rs",
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

    def test_backend_closure_preserves_build_inputs_without_operational_invalidations(self) -> None:
        """Documentation and Python tests cannot change compiler inputs or embedded Rust data."""
        inputs = (
            *cache.BACKEND_FILES,
            "Cargo.lock",
            "Cargo.toml",
            "rust-toolchain.toml",
            ".cargo/config.toml",
            "load_tests/bin/load_test.rs",
            "load_tests/clients/measurement.rs",
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
            ("cargo build --locked --all-features --bins", "cargo build --locked --bins", True),
            ('OPENSSL_STATIC: "1"', 'OPENSSL_STATIC: "0"', True),
            ("target/openssl-3.5.9", "target/openssl-3.5.10", True),
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
        openssl = "target/openssl-3.5.9/lib/libssl.a"
        for relative in (generated, openssl):
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            _ = path.write_text("generated fixture\n")
        output = self.backend_fixture(("src/main.rs", "vendor", generated, openssl))
        with patch.dict(os.environ, {"OPENSSL_DIR": str(self.root / "target/openssl-3.5.9")}):
            key = cache.cache_key(self.root, "backend")
            directory = self.root / "cache"
            cache.save(self.root, "backend", key, directory)
            for relative in (generated, openssl):
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
            self.assertIn("--all-features", arguments)
            if name == "browser":
                self.assertIn("--bins", arguments)
                self.assertNotIn("--all-targets", arguments)
                continue
            self.assertIn("--all-targets", arguments)
            self.assertIn("--profile", arguments)
            self.assertEqual(arguments[arguments.index("--profile") + 1], "test")
            execute = next(
                step for step in steps if string(step.get("run", "")).startswith("cargo test ")
            )
            self.assertEqual(
                string(execute, "run"),
                "cargo test --locked --all-features -- --include-ignored --test-threads=1",
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
