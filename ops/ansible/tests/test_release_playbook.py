"""Real Compose rendering of a candidate image selection; never contact a Docker daemon."""

import json
import shutil
import subprocess
import tempfile
import unittest
from copy import deepcopy
from pathlib import Path
from typing import override
from unittest.mock import patch

from ansible.plugins.loader import init_plugin_loader
from test_support import obj, string

# isort: split
import release_public as RELEASE  # noqa: N812 - name the helper under test.
import test_public_templates as TEMPLATES  # noqa: N812 - shared fixture module.
from release_artifact import sha256_file
from release_json import decode_json

# The optional Compose check renders local fixture files without daemon access.
# ruff: noqa: S603

ROOT = Path(__file__).resolve().parents[1]
PROJECT = ROOT.parents[1]


class ReleasePlaybookTests(unittest.TestCase):
    """Verify the candidate selection against the real Compose renderer offline."""

    @classmethod
    @override
    def setUpClass(cls) -> None:
        """Prepare the isolated fixture and register cleanup."""
        init_plugin_loader()

    @unittest.skipUnless(
        shutil.which("docker"), "Optional offline Compose renderer requires the Docker CLI"
    )
    def test_real_compose_preview_and_installed_selection_change_only_images_and_image_metadata(
        self,
    ) -> None:
        """Render the real Compose preview and confirm only application image selection changes."""
        docker = shutil.which("docker")
        if docker is None:
            self.skipTest("Docker CLI is unavailable; no daemon is required")
        version = subprocess.run(
            [docker, "compose", "version"],
            env=RELEASE.ENV,
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
        if version.returncode:
            self.skipTest("Docker Compose plugin is unavailable; no daemon is required")
        with tempfile.TemporaryDirectory(prefix="simplestchat-compose-release.") as temporary:
            root = Path(temporary).resolve()
            config, attempt = root / "config", root / "attempt"
            config.mkdir(mode=0o700)
            attempt.mkdir(mode=0o700)
            old_image = string(TEMPLATES.VALUES, "scpub_server_image")
            new_image = "sha256:" + "d" * 64
            for template, filename in (
                ("public-compose.yml.j2", "compose.public.yml"),
                ("public-app.env.j2", "app.env"),
                ("public-migration.env.j2", "migration.env"),
                ("public-proxy.env.j2", "proxy.env"),
            ):
                _ = (config / filename).write_text(
                    TEMPLATES.render(
                        template, scpub_config=str(config), scpub_root=str(root / "data")
                    )
                    + "\n"
                )
            _ = (config / "compose.base.yml").write_bytes(
                (PROJECT / "docker-compose.yml").read_bytes()
            )
            _ = (config / "Caddyfile").write_bytes((PROJECT / "Caddyfile").read_bytes())
            receipt = {
                "schemaVersion": 1,
                "caddyfileSha256": sha256_file(config / "Caddyfile"),
            }
            _ = (config / "browser-assets-installed.json").write_text(json.dumps(receipt))
            (config / "browser-assets-installed.json").chmod(0o600)
            runner = RELEASE.Runner(attempt)
            with (
                patch.object(RELEASE, "CONFIG", config),
                patch.object(RELEASE, "ROOT", root / "data"),
                patch.object(RELEASE, "ROOT_UID", config.stat().st_uid),
                patch.object(RELEASE, "DOCKER", [docker]),
            ):
                before = obj(
                    decode_json(
                        runner.compose("--profile", "maintenance", "config", "--format", "json")
                    )
                )
                hashes_before = {
                    service: runner.compose("config", "--hash", service)
                    for service in ("simplestchat", "postgres", "caddy")
                }
                preview, environment = RELEASE.candidate_selection(
                    runner, new_image, {"serverImage": old_image}
                )
                _ = (config / "compose.public.yml").write_bytes(preview)
                _ = (config / "app.env").write_bytes(environment)
                after = obj(
                    decode_json(
                        runner.compose("--profile", "maintenance", "config", "--format", "json")
                    )
                )
                expected = deepcopy(before)
                for service in ("simplestchat", "migrate"):
                    obj(expected, "services", service)["image"] = new_image
                obj(expected, "services", "simplestchat", "environment")["SIMPLESTCHAT_IMAGE"] = (
                    new_image
                )
                self.assertEqual(after, expected)
                for service in ("postgres", "caddy"):
                    self.assertEqual(
                        runner.compose("config", "--hash", service), hashes_before[service]
                    )
                self.assertNotEqual(
                    runner.compose("config", "--hash", "simplestchat"),
                    hashes_before["simplestchat"],
                )
                self.assertRegex(
                    hashes_before["simplestchat"].decode(), r"^simplestchat [a-f0-9]{64}\s*$"
                )


if __name__ == "__main__":
    _ = unittest.main()
