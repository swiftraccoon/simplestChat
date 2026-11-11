"""Release wiring and real Compose rendering; never contact a Docker daemon."""

import hashlib
import json
import shutil
import subprocess
import tempfile
import unittest
from copy import deepcopy
from functools import cached_property
from pathlib import Path
from typing import override
from unittest.mock import patch

from ansible.parsing.dataloader import DataLoader
from ansible.plugins.loader import init_plugin_loader
from ansible.template import Templar, trust_as_template
from test_support import array, at, obj, objects, string, strings, yaml_value

# isort: split
import release_preflight
import release_public as RELEASE  # noqa: N812 - name the helper under test.
import test_public_templates as TEMPLATES  # noqa: N812 - shared fixture module.
from release_artifact import sha256_file
from release_json import JsonObject, decode_json, json_value

# The optional Compose check renders local fixture files without daemon access.
# ruff: noqa: S603

ROOT = Path(__file__).resolve().parents[1]
PROJECT = ROOT.parents[1]


class ReleasePlaybookTests(unittest.TestCase):
    """Verify the release playbook contract offline."""

    @classmethod
    @override
    def setUpClass(cls) -> None:
        """Prepare the isolated fixture and register cleanup."""
        init_plugin_loader()

    @cached_property
    def play(self) -> JsonObject:
        """Read the release playbook once per isolated test case."""
        return obj(yaml_value((ROOT / "release.yml").read_text()), 0)

    @cached_property
    def tasks(self) -> list[JsonObject]:
        """Read ordered tasks, requiring each task to be a mapping."""
        return objects(self.play, "tasks")

    def command(self, action: str) -> JsonObject:
        """Find the reviewed task invoking one explicit CLI action."""
        return next(
            task
            for task in self.tasks
            if action in array(obj(task.get("ansible.builtin.command", {})).get("argv", []))
        )

    def pre_task(self, name: str) -> JsonObject:
        """Find one preflight by its stable task name."""
        return next(task for task in objects(self.play, "pre_tasks") if task["name"] == name)

    def test_github_fetch_is_bounded_controller_only_and_precedes_common_stage(self) -> None:
        """Verify github fetch is bounded controller only and precedes common stage."""
        fetch = self.command("--artifact-id")
        argv = strings(fetch, "ansible.builtin.command", "argv")
        self.assertEqual(
            argv,
            [
                "{{ ansible_playbook_python }}",
                "-B",
                "{{ playbook_dir }}/../../build/fetch-release.py",
                "--repository",
                "{{ scpub_release_repository }}",
                "--artifact-id",
                "{{ scpub_release_artifact_id }}",
                "--revision",
                "{{ scpub_release_revision }}",
                "--ci-run",
                "{{ scpub_release_ci_run }}",
                "--host",
                "{{ ansible_host }}",
                "--user",
                "{{ ansible_user }}",
                "--port",
                "{{ ansible_port | default(22) }}",
                "--identity",
                "{{ ansible_ssh_private_key_file }}",
                "--output-parent",
                "{{ (playbook_dir ~ '/../../results') | realpath }}",
                "--verified-directory",
                "{{ (scpub_validated_release.stdout | from_json).directory }}",
            ],
        )
        self.assertEqual(fetch["delegate_to"], "localhost")
        self.assertIs(fetch["become"], expr2=False)
        self.assertEqual(fetch["timeout"], 1080)
        self.assertEqual(
            fetch["when"], ["not ansible_check_mode", "scpub_release_artifact_id is defined"]
        )
        self.assertNotIn("--deploy", argv)
        self.assertNotIn("no_log", fetch)
        receiver = next(
            task
            for task in self.tasks
            if task.get("loop") == sorted(release_preflight.FETCH_HELPERS)
        )
        self.assertEqual(
            receiver["ansible.builtin.copy"],
            {
                "src": "{{ item }}",
                "dest": "/usr/local/libexec/simplestchat-public/{{ item }}",
                "owner": "root",
                "group": "root",
                "mode": "0644",
            },
        )
        self.assertEqual(
            receiver["when"],
            [
                "scpub_release_artifact_id is defined",
                "not (scpub_release_prepared | default(false) | bool)",
            ],
        )
        self.assertLess(self.tasks.index(receiver), self.tasks.index(fetch))
        self.assertLess(self.tasks.index(fetch), self.tasks.index(self.command("stage")))

    def test_python_helper_and_shared_module_are_installed_together(self) -> None:
        """Verify python helper and shared module are installed together."""
        for filename in ("release.yml", "public.yml"):
            play = obj(yaml_value((ROOT / filename).read_text()), 0)
            task = next(
                task
                for task in objects(play, "tasks")
                if task.get("name")
                == (
                    "Install the release command and shared artifact validator"
                    if filename == "release.yml"
                    else "Copy routine-release commands without starting a release"
                )
            )
            self.assertEqual(set(strings(task, "loop")), release_preflight.BASE_HELPERS)
            self.assertEqual(
                task["ansible.builtin.copy"],
                {
                    "src": "{{ item }}",
                    "dest": "/usr/local/libexec/simplestchat-public/{{ item }}",
                    "owner": "root",
                    "group": "root",
                    "mode": "0644",
                },
            )
        storage = next(
            task
            for task in self.tasks
            if task.get("loop")
            == [
                "{{ scpub_root }}/releases",
                "{{ scpub_root }}/releases/{{ scpub_release_revision }}",
            ]
        )
        self.assertEqual(at(storage, "ansible.builtin.file", "mode"), "0700")

    def test_real_ansible_lookup_preserves_source_bytes_and_selects_complete_helper_sets(
        self,
    ) -> None:
        """Verify real ansible lookup preserves source bytes and selects complete helper sets."""
        task = self.pre_task(
            "Verify the host and exact prepared helpers without broad fact gathering"
        )
        argv = strings(task, "ansible.builtin.command", "argv")
        base = {
            name: hashlib.sha256((ROOT / "files" / name).read_bytes()).hexdigest()
            for name in release_preflight.BASE_HELPERS
        }
        for prepared, github in ((None, False), (False, True), (True, False), (True, True)):
            values: JsonObject = {"playbook_dir": str(ROOT)}
            if prepared is not None:
                values["scpub_release_prepared"] = prepared
            if github:
                values["scpub_release_artifact_id"] = 123
            templar = Templar(loader=DataLoader(), variables=values)
            with self.subTest(prepared=prepared, github=github):
                code = string(json_value(templar.template(trust_as_template(argv[3]))))
                self.assertEqual(code.encode(), (ROOT / "files/release_preflight.py").read_bytes())
                expected = dict(base) if prepared else {}
                if prepared and github:
                    expected.update(
                        {
                            name: hashlib.sha256((ROOT / "files" / name).read_bytes()).hexdigest()
                            for name in release_preflight.FETCH_HELPERS
                        }
                    )
                result = string(json_value(templar.template(trust_as_template(argv[4]))))
                self.assertIsInstance(result, str)
                self.assertEqual(decode_json(result), expected)

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
