"""Verify source secret coverage and exact fixture reviews without exposing credentials."""

from __future__ import annotations

import base64
import hashlib
import json
import os
import shutil
import tempfile
import tomllib
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from test_support import ROOT, obj, objects, string

# isort: split
import security_check as check
import security_context as context
import security_findings as findings
import security_secrets as secrets
import security_tools as tools
from release_json import decode_json
from security_policy import read_exceptions
from security_secret_projection import PREFIX


def snapshot(
    root: Path, files: dict[str, bytes], *, configuration: str | None = None
) -> tuple[context.Context, Path, Path]:
    """Create the same authenticated source manifest consumed by the real gate."""
    source, output = root / "source", root / "output"
    source.mkdir()
    output.mkdir()
    manifest: dict[str, str] = {}
    for name, content in files.items():
        path = source / name
        path.parent.mkdir(parents=True, exist_ok=True)
        _ = path.write_bytes(content)
        manifest[name] = hashlib.sha256(content).hexdigest()
    _ = (output / "source-manifest.json").write_text(json.dumps(manifest))
    policy = root / "gitleaks.toml"
    _ = policy.write_text(
        configuration
        if configuration is not None
        else "title = 'Fixture'\nallowlists = []\n[extend]\nuseDefault = true\n"
    )
    return context.Context(root, output), source, policy


class SourceSecretTests(unittest.TestCase):
    """Projection cannot lose file identity or broaden a public fixture exemption."""

    def test_projection_covers_binary_lockfile_and_empty_inputs_with_original_hashes(self) -> None:
        """All selected file types receive projections tied to their unmodified original bytes."""
        files = {
            "vendor/fixture.bin": b"\x7fELF\0public-ascii-fixture\xff",
            "web/package-lock.json": b'{"name":"fixture"}\n',
            "empty": b"",
        }
        with tempfile.TemporaryDirectory() as temporary, patch.dict(os.environ, {}, clear=True):
            selected, source, policy = snapshot(Path(temporary), files)
            neutral, _ = secrets.neutral_snapshot(selected, source, policy, include_vendor=True)
            mapped = obj(decode_json((selected.output / "secret-paths.json").read_bytes()))
            coverage = obj(decode_json((selected.output / "secret-coverage.json").read_bytes()))
            self.assertEqual(coverage["files"], len(files))
            self.assertEqual(coverage["sourceBytes"], sum(map(len, files.values())))
            self.assertEqual(set(mapped), set(files))
            for name, content in files.items():
                entry_name = string(mapped, name)
                entry = obj(coverage, "entries", entry_name)
                projected = (neutral / entry_name).read_bytes()
                self.assertEqual(entry["path"], name)
                self.assertEqual(entry["sha256"], hashlib.sha256(content).hexdigest())
                self.assertEqual(entry["sourceBytes"], len(content))
                self.assertEqual(entry["projectionSha256"], hashlib.sha256(projected).hexdigest())
                self.assertEqual(entry["projectionBytes"], len(projected))
                self.assertTrue(set(projected) <= {*range(ord(" "), ord("~") + 1), 9, 10, 13})
                self.assertEqual((source / name).read_bytes(), content)
            self.assertIn(
                b"public-ascii-fixture",
                (neutral / string(mapped, "vendor/fixture.bin")).read_bytes(),
            )

    def test_changed_snapshot_never_creates_a_passing_coverage_manifest(self) -> None:
        """A source edit after inventory invalidates its original authenticated identity."""
        with tempfile.TemporaryDirectory() as temporary:
            selected, source, policy = snapshot(Path(temporary), {"file": b"original"})
            _ = (source / "file").write_bytes(b"changed")
            with self.assertRaisesRegex(tools.ToolError, "secret_snapshot_changed"):
                _ = secrets.neutral_snapshot(selected, source, policy)
            self.assertFalse((selected.output / "secret-coverage.json").exists())

    def test_size_and_count_budgets_fail_before_projection_creation(self) -> None:
        """An oversized tree is rejected completely instead of partially scanned or truncated."""
        cases = (
            ("MAX_FILES", 1, "secret_source_count"),
            ("MAX_SOURCE", 3, "secret_source_size"),
            ("MAX_TREE", 7, "secret_tree_size"),
        )
        for name, limit, reason in cases:
            with self.subTest(limit=name), tempfile.TemporaryDirectory() as temporary:
                selected, source, policy = snapshot(Path(temporary), {"a": b"data", "b": b"data"})
                with (
                    patch.object(secrets, name, limit),
                    self.assertRaisesRegex(tools.ToolError, reason),
                ):
                    _ = secrets.neutral_snapshot(selected, source, policy)
                self.assertFalse((selected.output / "secret-inputs").exists())

    def test_free_disk_accounts_for_every_prefix_before_projection_creation(self) -> None:
        """Available space must cover prefix expansion as well as all original input bytes."""
        with tempfile.TemporaryDirectory() as temporary:
            selected, source, policy = snapshot(Path(temporary), {"a": b"data", "b": b"data"})
            required = 8 + 2 * len(PREFIX) + secrets.DISK_RESERVE
            with (
                patch.object(shutil, "disk_usage", return_value=Mock(free=required - 1)),
                self.assertRaisesRegex(tools.ToolError, "secret_projection_disk_space"),
            ):
                _ = secrets.neutral_snapshot(selected, source, policy)
            self.assertFalse((selected.output / "secret-inputs").exists())
            with patch.object(shutil, "disk_usage", return_value=Mock(free=required)):
                _ = secrets.neutral_snapshot(selected, source, policy)
            coverage = obj(decode_json((selected.output / "secret-coverage.json").read_bytes()))
            self.assertEqual(coverage["projectionBytes"], required - secrets.DISK_RESERVE)

    def test_reviewed_fixture_values_remain_scoped_to_one_original_path(self) -> None:
        """The actual nine review fingerprints remain valid and only path selectors translate."""
        policy_path = ROOT / "security/gitleaks.toml"
        policy_text = policy_path.read_text()
        original = obj(tomllib.loads(policy_text))
        entries = objects(original, "allowlists")
        files = {
            string(entry, "description").removeprefix("Public fixture: "): b"fixture\n"
            for entry in entries
        }
        findings.gitleaks_configuration(policy_path, read_exceptions())
        with tempfile.TemporaryDirectory() as temporary, patch.dict(os.environ, {}, clear=True):
            selected, source, policy = snapshot(Path(temporary), files, configuration=policy_text)
            _, translated = secrets.neutral_snapshot(selected, source, policy, include_vendor=True)
            derived = objects(obj(tomllib.loads(translated.read_text())), "allowlists")
            mapped = obj(decode_json((selected.output / "secret-paths.json").read_bytes()))
            self.assertEqual(len(derived), len(entries))
            for before, after in zip(entries, derived, strict=True):
                name = string(before, "description").removeprefix("Public fixture: ")
                self.assertEqual(after, {**before, "paths": ["(^|/)" + string(mapped, name) + "$"]})

    def test_default_projection_never_reads_vendor_but_keeps_nested_first_party_paths(self) -> None:
        """The default scanner input omits top-level vendor before source-byte inspection."""
        files = {"vendor/unused.bin": b"vendor", "src/vendor/kept.rs": b"application"}
        with tempfile.TemporaryDirectory() as temporary:
            selected, source, policy = snapshot(Path(temporary), files)
            (source / "vendor/unused.bin").unlink()
            neutral, _ = secrets.neutral_snapshot(selected, source, policy)
            mapped = obj(decode_json((selected.output / "secret-paths.json").read_bytes()))
            coverage = obj(decode_json((selected.output / "secret-coverage.json").read_bytes()))
            self.assertEqual(set(mapped), {"src/vendor/kept.rs"})
            self.assertEqual(len(list(neutral.iterdir())), 1)
            self.assertFalse(coverage["includeVendor"])

    def test_private_git_index_excludes_vendor_without_changing_the_repository_index(self) -> None:
        """Real unstaged Git diffs preserve original app paths and never visit vendor blobs."""
        with tempfile.TemporaryDirectory() as temporary, patch.dict(os.environ, {}, clear=True):
            directory = Path(temporary)
            repo, output, source = (directory / name for name in ("repo", "output", "snapshot"))
            for path in (repo, output, source):
                path.mkdir()
            selected = context.Context(repo, output)
            git = context.executable("git")
            _ = selected.run("init", [git, "init", "--quiet"])
            for name in ("application.txt", "vendor/fixture.txt", "src/vendor/kept.txt"):
                path = repo / name
                path.parent.mkdir(parents=True, exist_ok=True)
                _ = path.write_text("before\n")
                target = source / name
                target.parent.mkdir(parents=True, exist_ok=True)
                _ = target.write_text("after\n")
            _ = selected.run("add", [git, "add", "."])
            _ = selected.run(
                "commit",
                [
                    git,
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "commit",
                    "--no-gpg-sign",
                    "--quiet",
                    "-m",
                    "fixture",
                ],
            )
            original = (repo / ".git/index").read_bytes()
            env = check.working_diff_environment(selected, source)
            _, diff = selected.run(
                "scoped-diff",
                [git, "-C", str(source), "diff", "-U0", "--no-ext-diff", "."],
                env_updates=env,
            )
            self.assertIn(b"a/application.txt", diff)
            self.assertIn(b"a/src/vendor/kept.txt", diff)
            self.assertNotIn(b"a/vendor/fixture.txt", diff)
            self.assertEqual((repo / ".git/index").read_bytes(), original)
            self.assertEqual((repo / "vendor/fixture.txt").read_text(), "before\n")

    def test_deep_native_suite_requires_local_opt_in(self) -> None:
        """Default deep retains application mutations and only explicit local mode adds native."""
        with tempfile.TemporaryDirectory() as temporary, patch.dict(os.environ, {}, clear=True):
            selected = context.Context(Path(temporary), Path(temporary))
            with (
                patch.object(check, "fast"),
                patch.object(selected, "run"),
                patch.object(check, "native_checks") as native,
                patch.object(check, "mutation_checks") as mutation,
            ):
                check.deep(selected, check.Options())
                native.assert_not_called()
                mutation.assert_called_once()
                with self.assertRaisesRegex(tools.ToolError, "native_scan_requires_include_vendor"):
                    check.deep(selected, check.Options(deep_check="native"))
                check.deep(selected, check.Options(deep_check="native", include_vendor=True))
                native.assert_called_once()

    def test_success_without_an_empty_findings_array_is_rejected(self) -> None:
        """Scanner exit zero cannot turn missing, malformed or nonempty evidence into success."""
        with tempfile.TemporaryDirectory() as temporary:
            selected = context.Context(Path(temporary), Path(temporary))
            for report in (b"", b"{}", b"null", b'[{"RuleID":"fixture"}]'):
                with (
                    self.subTest(report=report),
                    patch.object(context.Context, "run", return_value=(0, report)),
                    self.assertRaises((ValueError, tools.ToolError)),
                ):
                    check.clean_secret_scan(selected, "fixture", ["unused"])


@unittest.skipUnless(
    os.environ.get("SIMPLESTCHAT_GITLEAKS_ENGINE_TESTS") == "1",
    "Set SIMPLESTCHAT_GITLEAKS_ENGINE_TESTS=1 after installing the pinned Gitleaks engine.",
)
class SourceSecretEngineTests(unittest.TestCase):
    """Exercise the real authenticated detector; the fast gate always runs its own canary."""

    def test_binary_detector_selftest_finds_only_the_redacted_inert_canary(self) -> None:
        """Binary magic and an inline suppression cannot hide the projected ASCII fixture."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            selected = context.Context(root, root)
            options = [
                "--config",
                "replaced-by-selftest",
                "--redact=100",
                "--ignore-gitleaks-allow",
                "--gitleaks-ignore-path",
                os.devnull,
                "--log-level=warn",
                "--no-banner",
                "--report-format=json",
                "--report-path=-",
                "--max-target-megabytes=0",
                "--max-decode-depth=3",
            ]
            check.secret_selftest(selected, str(tools.tool_path("gitleaks")), options)
            self.assertEqual(selected.checks[-1]["exitStatus"], 10)
            self.assertEqual(selected.checks[-1]["expectedFinding"], "github-pat")

    def test_printable_document_magic_does_not_hide_projected_credentials(self) -> None:
        """An ASCII PDF signature needs neutralization as well as any nonprintable bytes."""
        canary = (
            "ghp_"
            + base64.b64encode(hashlib.sha512(b"inert printable document coverage only").digest())
            .decode()
            .replace("+", "x")
            .replace("/", "y")[:36]
        )
        with tempfile.TemporaryDirectory() as temporary:
            selected, source, policy = snapshot(
                Path(temporary),
                {"document.pdf": ("%PDF-1.7\nTOKEN=" + canary + "\n").encode()},
            )
            neutral, translated = secrets.neutral_snapshot(selected, source, policy)
            _, output = selected.run(
                "gitleaks-pdf-fixture",
                [
                    str(tools.tool_path("gitleaks")),
                    "dir",
                    str(neutral),
                    "--config",
                    str(translated),
                    "--redact=100",
                    "--gitleaks-ignore-path",
                    os.devnull,
                    "--ignore-gitleaks-allow",
                    "--max-target-megabytes=0",
                    "--no-banner",
                    "--log-level=warn",
                    "--report-format=json",
                    "--report-path=-",
                    "--exit-code=10",
                ],
                accepted=(10,),
            )
            entries = findings.list_value(decode_json(output))
            self.assertEqual(len(entries), 1)
            self.assertEqual(findings.object_value(entries[0])["RuleID"], "github-pat")
            self.assertNotIn(canary.encode(), output)


if __name__ == "__main__":
    _ = unittest.main()
