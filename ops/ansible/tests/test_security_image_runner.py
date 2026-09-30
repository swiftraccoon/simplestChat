"""Offline image controller checks model actual owned-container lifecycle boundaries."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from typing import TYPE_CHECKING
from unittest.mock import patch

from test_support import ROOT

# isort: split
import release_build
import security_image as image
from release_json import decode_json, object_value, string_value
from security_tools import ToolError

if TYPE_CHECKING:
    from collections.abc import Sequence

    from release_json import JsonObject

_ = ROOT  # Import initializes maintained helper paths.
READ_ONLY = 0o444
BASE = "sha256:" + "a" * 64
IDENTIFIER = "b" * 64


class Engine:
    """Record only allowed scanner lifecycle operations, never emulate an application."""

    def __init__(self, *, fail: bool = False, changed: bool = False) -> None:
        """Select a timeout or foreign-resource regression explicitly."""
        self.calls: list[list[str]] = []
        self.owner: str = ""
        self.fail: bool = fail
        self.changed: bool = changed

    def command(
        self, argv: Sequence[str], *, timeout: int = 30, allow_failure: bool = False
    ) -> tuple[int, str]:
        """Require exact current command shapes; unsupported behavior fails the fixture."""
        del timeout, allow_failure
        call = list(argv)
        self.calls.append(call)
        if call[0] == "run":
            self.owner = call[call.index("--label") + 1].split("=", 1)[1]
            if self.fail:
                reason = "owned fixture timeout"
                raise release_build.BuildError(reason)
            return 0, "scanner output"
        if call[:2] == ["container", "inspect"] and "--format" in call:
            return 0, json.dumps(
                {
                    "id": IDENTIFIER,
                    "image": BASE,
                    "owner": "foreign" if self.changed else self.owner,
                    "state": "exited",
                }
            )
        if call[:3] == ["container", "rm", "--force"]:
            if call[-1] != IDENTIFIER:
                raise AssertionError(call)
            return 0, IDENTIFIER
        if call == ["ps", "--all", "--quiet", "--no-trunc", "--filter", "id=" + IDENTIFIER]:
            return 0, ""
        raise AssertionError(call)


class ImageRunnerTests(unittest.TestCase):
    """Malformed selection, partial failures and cleanup ambiguity remain failures."""

    def test_offline_scanner_has_explicit_limits_and_no_socket_or_home_mount(self) -> None:
        """Inspect actual generated argv and exact owned cleanup IDs."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sandbox = image.Sandbox(
                ["/usr/bin/docker"], {}, root, root / "tools", BASE, "linux/amd64"
            )
            engine = Engine()
            with patch.object(sandbox, "command", side_effect=engine.command):
                self.assertEqual(
                    sandbox.run(
                        "syft",
                        ["scan", "docker-archive:/input/image.tar"],
                        destination=root / "out",
                        mounts={"/input": root / "input"},
                    )[0],
                    0,
                )
            argv = engine.calls[0]
            self.assertEqual(argv[argv.index("--network") + 1], "none")
            self.assertIn("--read-only", argv)
            self.assertIn("--cap-drop=ALL", argv)
            self.assertIn("--security-opt=no-new-privileges", argv)
            self.assertIn("--pids-limit=128", argv)
            self.assertIn("--memory=2g", argv)
            self.assertIn("--memory-swap=2g", argv)
            self.assertIn("--cpus=2", argv)
            self.assertEqual(argv[argv.index("--entrypoint") + 1], "/tools/syft")
            self.assertNotIn("docker.sock", " ".join(argv))
            self.assertNotIn("/root", " ".join(argv))
            self.assertIn(["container", "rm", "--force", IDENTIFIER], engine.calls)

    def test_online_database_preparation_mounts_no_image(self) -> None:
        """Online preparation has only verified tools and its fresh empty output directory."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sandbox = image.Sandbox(
                ["/usr/bin/docker"], {}, root, root / "tools", BASE, "linux/amd64"
            )
            engine = Engine()
            with patch.object(sandbox, "command", side_effect=engine.command):
                _ = sandbox.run("grype", ["db", "update"], destination=root / "db", online=True)
            argv = engine.calls[0]
            self.assertEqual(argv[argv.index("--network") + 1], "bridge")
            self.assertEqual(argv.count("--mount"), 2)
            self.assertNotIn("/input", " ".join(argv))
            self.assertNotIn("/layers", " ".join(argv))

    def test_timeout_cleans_only_proven_owned_container_and_preserves_failure(self) -> None:
        """A successful forced removal cannot turn a timed-out scanner into a pass."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sandbox = image.Sandbox(
                ["/usr/bin/docker"], {}, root, root / "tools", BASE, "linux/amd64"
            )
            engine = Engine(fail=True)
            with (
                patch.object(sandbox, "command", side_effect=engine.command),
                self.assertRaises(release_build.BuildError),
            ):
                _ = sandbox.run("syft", ["--help"], destination=root / "out")
            self.assertIn(["container", "rm", "--force", IDENTIFIER], engine.calls)
            self.assertIsNone(sandbox.runner.healthy)

    def test_changed_identity_is_retained_instead_of_deleting_by_name(self) -> None:
        """A conflicting ownership label prevents stop/remove despite a familiar name."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sandbox = image.Sandbox(
                ["/usr/bin/docker"], {}, root, root / "tools", BASE, "linux/amd64"
            )
            engine = Engine(changed=True)
            with (
                patch.object(sandbox, "command", side_effect=engine.command),
                self.assertRaises(ToolError),
            ):
                _ = sandbox.run("syft", ["--help"], destination=root / "out")
            self.assertFalse(any(call[:2] == ["container", "rm"] for call in engine.calls))

    def test_output_limits_reject_large_files_links_and_special_entries(self) -> None:
        """The parent actively bounds writable mounts independently of scanner exit."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            file = root / "output"
            _ = file.write_bytes(b"12345")
            self.assertTrue(image.directory_size(root, 5))
            self.assertFalse(image.directory_size(root, 4))
            (root / "link").symlink_to(file)
            self.assertFalse(image.directory_size(root, 100))

    def test_export_requires_exact_id_and_passed_matching_outcome(self) -> None:
        """A tag or a different export cannot be substituted for selected image bytes."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            args = image.Options(image_id="latest", artifact_dir=root)
            with self.assertRaises(ToolError):
                _ = image.bind_archive(args)
            args.image_id = BASE
            _ = (root / "outcome.json").write_text(
                json.dumps({"passed": True, "exportedImageId": "sha256:" + "c" * 64})
            )
            with self.assertRaises(ToolError):
                _ = image.bind_archive(args)

    def test_selected_image_binds_raw_config_even_when_layers_match(self) -> None:
        """Matching layer content cannot stand in for the selected configuration identity."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sandbox = image.Sandbox(
                ["/usr/bin/docker"], {}, root, root / "tools", BASE, "linux/amd64"
            )
            args = image.Options(image_id=BASE)
            observed = {"id": BASE, "layers": ["sha256:" + "c" * 64], "architecture": "amd64"}
            for config_hash in ("a" * 64, "d" * 64):
                with self.subTest(config_hash=config_hash):
                    report = {
                        "layers": [{"diffId": observed["layers"][0]}],
                        "imageId": "sha256:" + config_hash,
                        "configSha256": config_hash,
                    }
                    _ = (root / "report.json").write_text(json.dumps(report))
                    with patch.object(sandbox, "command", return_value=(0, json.dumps(observed))):
                        if config_hash == "a" * 64:
                            self.assertEqual(
                                image.selected_image(sandbox, args, root)["archiveConfigSha256"],
                                config_hash,
                            )
                        else:
                            with self.assertRaisesRegex(ToolError, "archive_mismatch"):
                                _ = image.selected_image(sandbox, args, root)

    def test_native_binding_rejects_mismatched_executable(self) -> None:
        """Build-time evidence must bind the actual file inspected by ELF, not just its path."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "rootfs/usr/share/simplestchat/native-components.json"
            path.parent.mkdir(parents=True)
            _ = path.write_text(json.dumps({"binary": {"sha256": "a" * 64}}))
            with self.assertRaises(ToolError):
                _ = image.native_binding(root, {"binarySha256": "b" * 64})

    def test_secret_inputs_flatten_every_regular_file_and_retain_exact_paths(self) -> None:
        """Package and binary extensions must not invoke scanner path exclusions."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            layers = root / "layers"
            files = (
                "000/node_modules/package/key.bin",
                "001/package-lock.json",
                "image-config.json",
            )
            for name in files:
                path = layers / name
                path.parent.mkdir(parents=True, exist_ok=True)
                _ = path.write_text("inert content")
            inputs = image.secret_bundle(layers, root)
            paths = object_value(decode_json((root / "secret-paths.json").read_text()))
            self.assertEqual(
                {string_value(object_value(value)["path"]) for value in paths.values()}, set(files)
            )
            self.assertEqual(len(list(inputs.iterdir())), len(files))
            self.assertTrue(all(path.name.startswith("content-") for path in inputs.iterdir()))
            self.assertTrue(
                all(path.stat().st_mode & 0o777 == READ_ONLY for path in inputs.iterdir())
            )

    def test_secret_detector_canary_requires_exact_real_finding(self) -> None:
        """Empty, missing and unrelated reports cannot claim the configured detector works."""
        finding: JsonObject = {
            "File": "/layers/content-000000",
            "RuleID": "github-pat",
            "StartLine": 1,
        }
        cases: tuple[tuple[int, list[JsonObject] | None, bool], ...] = (
            (image.FINDINGS_EXIT, [finding], True),
            (0, [], False),
            (image.FINDINGS_EXIT, [], False),
            (image.FINDINGS_EXIT, [{**finding, "RuleID": "unrelated"}], False),
            (image.FINDINGS_EXIT, None, False),
        )
        for status, report, passed in cases:
            with (
                self.subTest(status=status, report=report),
                tempfile.TemporaryDirectory() as temporary,
            ):
                root = Path(temporary)
                sandbox = image.Sandbox(
                    ["/usr/bin/docker"], {}, root, root / "tools", BASE, "linux/amd64"
                )
                output = root / "secret-canary-output"
                output.mkdir()
                if report is not None:
                    _ = (output / "gitleaks.json").write_text(json.dumps(report))
                with patch.object(sandbox, "run", return_value=(status, "")) as run:
                    if passed:
                        image.secret_selftest(sandbox)
                        self.assertTrue(
                            object_value(decode_json((root / "secret-selftest.json").read_text()))[
                                "passed"
                            ]
                        )
                    else:
                        with self.assertRaises((ToolError, FileNotFoundError)):
                            image.secret_selftest(sandbox)
                        self.assertFalse((root / "secret-selftest.json").exists())
                    self.assertEqual(run.call_args.args, ("gitleaks", image.secret_arguments()))
