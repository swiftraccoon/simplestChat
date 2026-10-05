"""Keep extractor dependency reuse bounded and independent of analysis results."""

from __future__ import annotations

import io
import json
import os
import platform
import sys
import tempfile
import unittest
from pathlib import Path
from typing import override
from unittest.mock import patch

from test_support import ROOT, objects, string, yaml_value

# isort: split
import ci_verified
import security_codeql_cargo as cargo
import security_codeql_local as local
from security_tools import ToolError

# isort: split
from release_json import decode_json, object_value


class CargoCacheTests(unittest.TestCase):
    """Exercise real cache files without invoking a compiler or trusting a scan receipt."""

    directory: Path = Path()
    key: str = "codeql-rust-cargo-v1-main-linux-x86_64-" + "a" * 64

    @override
    def setUp(self) -> None:
        """Keep every private fixture in the repository's ignored results directory."""
        temporary = tempfile.TemporaryDirectory(prefix="cargo-cache-", dir=ROOT / "results")
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)

    def test_dependency_identity_changes_only_with_build_inputs(self) -> None:
        """Application edits require fresh analysis while unchanged dependencies can stay warm."""
        dependencies = (
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            ".cargo/config.toml",
            "nested/build.rs",
            "vendor/example/src/lib.rs",
            "build/security_codeql_cargo.py",
            "security/codeql-toolchain.json",
        )
        unrelated = ("src/main.rs", "README.md", "security/exceptions.json")
        listing = b""
        for name in (*dependencies, *unrelated):
            path = self.directory / name
            path.parent.mkdir(parents=True, exist_ok=True)
            _ = path.write_text("original")
            listing += b"100644 " + b"a" * 40 + b" 0\t" + name.encode() + b"\0"
        source = self.directory / "source"
        with (
            patch.object(cargo, "command", return_value=listing),
            patch.object(cargo, "tool_identity", return_value="pinned compiler") as tool,
            patch.object(ci_verified, "runner_identity", return_value="runner") as runner,
            patch.dict(os.environ, {"GITHUB_REF": "refs/heads/main", "GITHUB_EVENT_NAME": "push"}),
        ):
            original = cargo.cache_key(self.directory, source)
            for name in (*dependencies, *unrelated):
                with self.subTest(path=name):
                    path = self.directory / name
                    _ = path.write_text("changed")
                    changed = cargo.cache_key(self.directory, source)
                    self.assertEqual(original == changed, name in unrelated)
                    _ = path.write_text("original")
            self.assertNotEqual(original, cargo.cache_key(self.directory, source / "different"))
            tool.return_value = "changed compiler"
            self.assertNotEqual(original, cargo.cache_key(self.directory, source))
            tool.return_value = "pinned compiler"
            runner.return_value = "changed runner"
            self.assertNotEqual(original, cargo.cache_key(self.directory, source))
            runner.return_value = "runner"
            with patch.dict(os.environ, {"GITHUB_EVENT_NAME": "pull_request"}):
                self.assertNotEqual(original, cargo.cache_key(self.directory, source))
            with patch.object(platform, "machine", return_value="another_architecture"):
                self.assertNotEqual(original, cargo.cache_key(self.directory, source))

    def test_prepare_creates_private_receipt_and_reuses_only_exact_namespace(self) -> None:
        """The compilation receipt binds source path and dependency key, not a policy verdict."""
        source, directory = self.directory / "source", self.directory / "cache"
        source.mkdir()
        with patch.object(cargo, "cache_key", return_value=self.key) as key:
            target = cargo.prepare(ROOT, source, directory)
            self.assertEqual(target, directory / "target")
            self.assertEqual(directory.stat().st_mode & 0o777, 0o700)
            self.assertEqual(target.stat().st_mode & 0o777, 0o700)
            receipt = decode_json((directory / "receipt.json").read_bytes())
            self.assertEqual(
                receipt, {"schemaVersion": 1, "key": self.key, "sourceRoot": str(source)}
            )
            _ = (target / "compiler-output").write_bytes(b"build output")
            (target / "compiler-output-alias").hardlink_to(target / "compiler-output")
            self.assertEqual(cargo.prepare(ROOT, source, directory), target)
            self.assertEqual(cargo.payload_bytes(target), len(b"build output"))
            key.return_value = "codeql-rust-cargo-v1-main-linux-x86_64-" + "b" * 64
            with self.assertRaisesRegex(ToolError, "codeql_cargo_receipt"):
                _ = cargo.prepare(ROOT, source, directory)

    def test_incomplete_alias_or_public_cache_cannot_be_consumed(self) -> None:
        """Restored build outputs are rejected before any executable artifact is loaded."""
        source = self.directory / "source"
        source.mkdir()
        with patch.object(cargo, "cache_key", return_value=self.key):
            directory = self.directory / "cache"
            target = cargo.prepare(ROOT, source, directory)
            target.chmod(0o755)
            with self.assertRaisesRegex(ToolError, "codeql_cargo_directory_permissions"):
                _ = cargo.prepare(ROOT, source, directory)
            target.chmod(0o700)
            _ = (directory / "unexpected").write_text("partial")
            with self.assertRaisesRegex(ToolError, "codeql_cargo_inventory"):
                _ = cargo.prepare(ROOT, source, directory)
            (directory / "unexpected").unlink()
            alias = self.directory / "alias"
            alias.symlink_to(directory, target_is_directory=True)
            with self.assertRaisesRegex(ToolError, "codeql_cargo_path"):
                _ = cargo.prepare(ROOT, source, alias)
            (target / "link").symlink_to(directory / "receipt.json")
            with self.assertRaisesRegex(ToolError, "codeql_cargo_entry_type"):
                _ = cargo.prepare(ROOT, source, directory)

    def test_oversized_output_is_not_published_or_consumed(self) -> None:
        """A successful analysis can skip a large build cache without weakening its checks."""
        source, directory = self.directory / "source", self.directory / "cache"
        source.mkdir()
        with patch.object(cargo, "cache_key", return_value=self.key):
            target = cargo.prepare(ROOT, source, directory)
            _ = (target / "large-output").write_bytes(b"12")
            output = io.StringIO()
            with (
                patch.object(cargo, "MAX_BYTES", 1),
                patch.object(sys, "stdout", output),
                patch.object(
                    sys,
                    "argv",
                    [
                        "cargo-cache",
                        "--source-root",
                        str(source),
                        "--publication-check",
                        str(directory),
                    ],
                ),
            ):
                self.assertEqual(cargo.main(), 0)
                self.assertEqual(output.getvalue(), "cacheable=false\nbytes=2\nlimit-bytes=1\n")
                with self.assertRaisesRegex(ToolError, "codeql_cargo_size"):
                    _ = cargo.prepare(ROOT, source, directory)
            with (
                patch.object(cargo, "MAX_ENTRIES", 0),
                self.assertRaisesRegex(ToolError, "codeql_cargo_entries"),
            ):
                _ = cargo.payload_bytes(target)

    def test_workflow_reuses_dependencies_only_for_fresh_rust_extraction(self) -> None:
        """No cached dependency tree skips extraction, policy or the bounded publication check."""
        workflow = yaml_value(
            (ROOT / ".github/workflows/codeql.yml").read_text(), scalars_as_strings=True
        )
        steps = objects(workflow, "jobs", "source-analysis", "steps")
        restore = next(step for step in steps if step.get("id") == "rust-cargo")
        self.assertIn("steps.rust-database.outputs.cache-hit != 'true'", string(restore, "if"))
        self.assertNotIn("restore-keys", string(restore, "with", "key"))
        runner = next(step for step in steps if step.get("id") == "source-analysis")
        self.assertIn('--rust-cargo-cache "$RUNNER_TEMP/codeql-rust-cargo"', string(runner, "run"))
        size = next(step for step in steps if step.get("id") == "rust-cargo-size")
        self.assertIn(
            "steps.source-analysis.outputs.database-cache-ready == 'true'", string(size, "if")
        )
        save = next(
            step
            for step in steps
            if step.get("name") == "Save bounded Rust extraction build output"
        )
        self.assertIn("steps.rust-cargo-size.outputs.cacheable == 'true'", string(save, "if"))
        self.assertLess(steps.index(size), steps.index(save))
        self.assertNotIn("continue-on-error", runner)

    def test_only_content_keyed_dependency_mtimes_are_stabilized(self) -> None:
        """Fresh snapshots must not rebuild identical vendored crates or hide application edits."""
        for name in ("vendor/example/src/lib.rs", "build/pip-constraints.txt", "src/main.rs"):
            path = self.directory / name
            path.parent.mkdir(parents=True, exist_ok=True)
            _ = path.write_text("fixture")
            os.utime(path, (1234567890, 1234567890))
        cargo.stabilize_dependencies(self.directory)
        self.assertEqual(
            (self.directory / "vendor/example/src/lib.rs").stat().st_mtime,
            cargo.DEPENDENCY_MTIME,
        )
        self.assertEqual(
            (self.directory / "build/pip-constraints.txt").stat().st_mtime,
            cargo.DEPENDENCY_MTIME,
        )
        self.assertEqual((self.directory / "src/main.rs").stat().st_mtime, 1234567890)

    def test_original_extractor_counts_survive_summary_without_source_excerpts(self) -> None:
        """Hosted evidence must expose existing Rust warnings and coverage on hits and misses."""
        metrics = [
            {"ruleId": "rust/summary/summary-statistics", "message": {"text": label}, "value": 10}
            for label in sorted(local.RUST_EXTRACTION_STATISTICS)
        ]
        encoded = json.dumps({"properties": {"metricResults": metrics}})
        run = object_value(decode_json(encoded))
        expected = dict.fromkeys(local.RUST_EXTRACTION_STATISTICS, 10)
        self.assertEqual(local.extraction_statistics(run, "rust"), expected)
        self.assertEqual(local.extraction_statistics(run, "python"), {})
        with self.assertRaisesRegex(ToolError, "metrics_missing"):
            _ = local.extraction_statistics({}, "rust")


if __name__ == "__main__":
    _ = unittest.main()
