"""Release wiring and real Compose rendering; never contact a Docker daemon."""

from copy import deepcopy
import importlib.util
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import yaml
from jinja2 import Environment, StrictUndefined, UndefinedError

ROOT = Path(__file__).resolve().parents[1]
PROJECT = ROOT.parents[1]


def module(name, path):
    specification = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(result)
    return result


TEMPLATES = module("release_playbook_templates", ROOT / "tests/test_public_templates.py")
sys.path.insert(0, str(ROOT / "files"))
try:
    RELEASE = module("release_playbook_runtime", ROOT / "files/release-public.py")
finally:
    sys.path.pop(0)


class ReleasePlaybookTests(unittest.TestCase):
    def setUp(self):
        self.play = yaml.safe_load((ROOT / "release.yml").read_text())[0]
        self.tasks = self.play["tasks"]

    def command(self, action):
        return next(task for task in self.tasks
                    if action in task.get("ansible.builtin.command", {}).get("argv", []))

    def pre_task(self, name):
        return next(task for task in self.play["pre_tasks"] if task["name"] == name)

    @staticmethod
    def assertions_pass(task, values):
        environment = Environment(undefined=StrictUndefined)
        environment.tests["match"] = lambda value, pattern: re.match(pattern, str(value)) is not None
        try:
            return all(environment.compile_expression(expression)(**values)
                       for expression in task["ansible.builtin.assert"]["that"])
        except (UndefinedError, TypeError):
            return False

    def github_values(self):
        return {
            "scpub_release_repository": "example/simplestChat",
            "scpub_release_artifact_id": 1234,
            "scpub_release_expected_revision": "a" * 40,
            "scpub_release_ci_run": 5678,
            "ansible_host": "chat.example.test",
            "ansible_user": "root",
            "ansible_ssh_private_key_file": "/private/controller key",
        }

    def test_artifact_sources_are_mutually_exclusive_and_select_their_own_revision(self):
        expression = "(scpub_release_directory is defined) != (scpub_release_artifact_id is defined)"
        assertions = self.play["pre_tasks"][0]["ansible.builtin.assert"]["that"]
        self.assertIn(expression, assertions)
        evaluate = Environment(undefined=StrictUndefined).compile_expression(expression)
        self.assertFalse(evaluate())
        self.assertTrue(evaluate(scpub_release_directory="/release"))
        self.assertTrue(evaluate(scpub_release_artifact_id=1234))
        self.assertFalse(evaluate(scpub_release_directory="/release", scpub_release_artifact_id=1234))
        local = self.pre_task("Select the verified local source revision")
        remote = self.pre_task("Select the explicitly pinned GitHub revision")
        self.assertEqual(local["when"], "scpub_release_directory is defined")
        self.assertEqual(remote["when"], "scpub_release_artifact_id is defined")
        self.assertEqual(local["ansible.builtin.set_fact"]["scpub_release_revision"],
                         "{{ (scpub_validated_release.stdout | from_json).revision }}")
        self.assertEqual(remote["ansible.builtin.set_fact"]["scpub_release_revision"],
                         "{{ scpub_release_expected_revision }}")

    def test_github_source_requires_exact_identities_and_explicit_supported_ssh(self):
        task = self.pre_task("Require an exact GitHub source and supported key-based SSH connection")
        self.assertEqual(task["when"], "scpub_release_artifact_id is defined")
        self.assertTrue(self.assertions_pass(task, self.github_values()))
        self.assertTrue(self.assertions_pass(task, dict(self.github_values(), ansible_user="deploy",
                                                      ansible_host="chat-alias", ansible_port="2222")))
        for key in self.github_values():
            values = self.github_values()
            del values[key]
            with self.subTest(missing=key):
                self.assertFalse(self.assertions_pass(task, values))
        for key, value in (
            ("scpub_release_repository", "https://github.com/example/repo"),
            ("scpub_release_repository", "example/repo/extra"), ("scpub_release_repository", "example/"),
            ("scpub_release_artifact_id", 0), ("scpub_release_artifact_id", True),
            ("scpub_release_artifact_id", "01"), ("scpub_release_ci_run", -1), ("scpub_release_ci_run", "1.0"),
            ("scpub_release_expected_revision", "main"), ("scpub_release_expected_revision", "A" * 40),
            ("ansible_connection", "local"), ("ansible_connection", "paramiko"),
            ("ansible_host", ""), ("ansible_host", "2001:db8::1"), ("ansible_host", "-oProxyCommand=command"),
            ("ansible_user", ""), ("ansible_ssh_private_key_file", "~/.ssh/id_ed25519"),
            ("ansible_port", 0), ("ansible_port", 65536), ("ansible_port", True),
            ("ansible_password", ""), ("ansible_ssh_pass", ""), ("ansible_ssh_password", ""),
            ("ansible_become_password", ""), ("ansible_become_pass", ""),
            ("ansible_ssh_common_args", "-J jump.example.test"), ("ansible_ssh_extra_args", "-F custom.conf"),
        ):
            with self.subTest(key=key, value=value):
                self.assertFalse(self.assertions_pass(task, dict(self.github_values(), **{key: value})))

    def test_github_fetch_is_bounded_controller_only_and_precedes_common_stage(self):
        fetch = self.command("--artifact-id")
        argv = fetch["ansible.builtin.command"]["argv"]
        self.assertEqual(argv, [
            "{{ ansible_playbook_python }}", "-B", "{{ playbook_dir }}/../../build/fetch-release.py",
            "--repository", "{{ scpub_release_repository }}", "--artifact-id", "{{ scpub_release_artifact_id }}",
            "--revision", "{{ scpub_release_revision }}", "--ci-run", "{{ scpub_release_ci_run }}",
            "--host", "{{ ansible_host }}", "--user", "{{ ansible_user }}",
            "--port", "{{ ansible_port | default(22) }}", "--identity", "{{ ansible_ssh_private_key_file }}",
            "--output-parent", "{{ (playbook_dir ~ '/../../results') | realpath }}",
        ])
        self.assertEqual(fetch["delegate_to"], "localhost")
        self.assertIs(fetch["become"], False)
        self.assertEqual(fetch["timeout"], 450)
        self.assertEqual(fetch["when"], ["not ansible_check_mode", "scpub_release_artifact_id is defined"])
        self.assertNotIn("--deploy", argv)
        self.assertNotIn("no_log", fetch)
        receiver = next(task for task in self.tasks
                        if task.get("ansible.builtin.copy", {}).get("src") == "fetch-release.py")
        self.assertEqual(receiver["ansible.builtin.copy"], {
            "src": "fetch-release.py", "dest": "/usr/local/libexec/simplestchat-public/fetch-release.py",
            "owner": "root", "group": "root", "mode": "0644",
        })
        self.assertEqual(receiver["when"], "scpub_release_artifact_id is defined")
        self.assertLess(self.tasks.index(receiver), self.tasks.index(fetch))
        self.assertLess(self.tasks.index(fetch), self.tasks.index(self.command("stage")))

    def test_stage_is_default_and_deployment_is_an_explicit_separate_job(self):
        stage = self.command("stage")
        deploy = self.command("deploy")
        self.assertLess(self.tasks.index(stage), self.tasks.index(deploy))
        self.assertEqual(stage["when"], "not ansible_check_mode")
        self.assertEqual(deploy["when"], [
            "not ansible_check_mode", "scpub_release_deploy | default(false) | bool",
        ])
        for action, task, runtime, grace in (
            ("stage", stage, 600, 60), ("deploy", deploy, 900, 240),
        ):
            argv = task["ansible.builtin.command"]["argv"]
            self.assertEqual(argv[0], "systemd-run")
            self.assertIn("--wait", argv)
            self.assertIn(f"--property=RuntimeMaxSec={runtime}", argv)
            self.assertIn(f"--property=TimeoutStopSec={grace}", argv)
            self.assertEqual(argv[-5:], [
                "/usr/bin/python3", "-B", "/usr/local/libexec/simplestchat-public/release-public.py",
                action, "{{ scpub_release_revision }}",
            ])
        self.assertEqual(self.play["serial"], 1)

    def test_routine_release_has_no_host_maintenance_or_configuration_rendering(self):
        allowed = {"ansible.builtin.assert", "ansible.builtin.command", "ansible.builtin.copy",
                   "ansible.builtin.file", "ansible.builtin.set_fact", "ansible.builtin.stat"}
        for task in self.play["pre_tasks"] + self.tasks:
            modules = [key for key in task if key.startswith("ansible.builtin.")]
            self.assertEqual(len(modules), 1)
            self.assertIn(modules[0], allowed)
            argv = task.get("ansible.builtin.command", {}).get("argv", [])
            self.assertTrue({"apt", "reboot", "stop", "restart", "up", "down", "build", "pull"}.isdisjoint(argv))
        assertions = self.play["pre_tasks"][0]["ansible.builtin.assert"]["that"]
        self.assertIn("scpub_enabled | bool", assertions)
        local_source = self.pre_task("Require an absolute local artifact directory")
        self.assertEqual(local_source["when"], "scpub_release_directory is defined")
        self.assertIn("scpub_release_directory is match('^/')", local_source["ansible.builtin.assert"]["that"])
        self.assertIn("scpub_config == '/etc/simplestchat-public'", assertions)
        self.assertIn("scpub_root == '/srv/simplestchat-public'", assertions)
        self.assertIn("ansible_facts.architecture == 'x86_64'", assertions)
        selection = next(task for task in self.play["pre_tasks"]
                         if task.get("register") == "scpub_existing_selection")
        self.assertFalse(selection["ansible.builtin.stat"]["follow"])

    def test_no_overwrite_is_paired_with_exact_controller_and_destination_identity(self):
        validation = next(task for task in self.play["pre_tasks"]
                          if task.get("register") == "scpub_validated_release")
        self.assertEqual(validation["delegate_to"], "localhost")
        self.assertIs(validation["become"], False)
        self.assertIs(validation["changed_when"], False)
        self.assertEqual(validation["when"], "scpub_release_directory is defined")
        code = validation["ansible.builtin.command"]["argv"][3]
        self.assertIn("validate_manifest(root / 'release.json')", code)
        self.assertIn("verify_archive(root / 'image.tar', manifest)", code)
        self.assertIn("'image.tar': manifest['archiveSha256']", code)
        self.assertIn("'release.json': sha256_file(root / 'release.json')", code)
        transfer = next(task for task in self.tasks if task.get("loop") == ["release.json", "image.tar"]
                        and "ansible.builtin.copy" in task)
        copy = transfer["ansible.builtin.copy"]
        self.assertIs(copy["force"], False)
        self.assertEqual(copy["dest"], "{{ scpub_root }}/releases/{{ scpub_release_revision }}/{{ item }}")
        self.assertEqual((copy["owner"], copy["group"], copy["mode"]), ("root", "root", "0600"))
        inspection = next(task for task in self.tasks if task.get("register") == "scpub_transferred_files")
        self.assertEqual(inspection["ansible.builtin.stat"]["checksum_algorithm"], "sha256")
        self.assertIs(inspection["ansible.builtin.stat"]["follow"], False)
        comparison = next(task for task in self.tasks if "ansible.builtin.assert" in task)
        self.assertEqual(comparison["loop"], "{{ scpub_transferred_files.results | default([]) }}")
        assertions = comparison["ansible.builtin.assert"]["that"]
        for assertion in ["item.stat.isreg | default(false)", "item.stat.uid == 0", "item.stat.mode == '0600'",
                          "item.stat.checksum == (scpub_validated_release.stdout | from_json)[item.item]"]:
            self.assertIn(assertion, assertions)
        self.assertLess(self.tasks.index(transfer), self.tasks.index(inspection))
        self.assertLess(self.tasks.index(inspection), self.tasks.index(comparison))
        self.assertLess(self.tasks.index(comparison), self.tasks.index(self.command("stage")))
        for task in (transfer, inspection, comparison):
            self.assertEqual(task["when"], "scpub_release_directory is defined")

    def test_python_helper_and_shared_module_are_installed_together(self):
        for filename in ("release.yml", "public.yml"):
            play = yaml.safe_load((ROOT / filename).read_text())[0]
            task = next(task for task in play["tasks"]
                        if task.get("loop") == ["release-public.py", "release_artifact.py", "reboot-public.py"])
            self.assertEqual(task["ansible.builtin.copy"], {
                "src": "{{ item }}", "dest": "/usr/local/libexec/simplestchat-public/{{ item }}",
                "owner": "root", "group": "root", "mode": "0644",
            })
        storage = next(task for task in self.tasks if task.get("loop") == [
            "{{ scpub_root }}/releases", "{{ scpub_root }}/releases/{{ scpub_release_revision }}",
        ])
        self.assertEqual(storage["ansible.builtin.file"]["mode"], "0700")

    @unittest.skipUnless(shutil.which("docker"), "Optional offline Compose renderer requires the Docker CLI")
    def test_real_compose_preview_and_installed_selection_change_only_images_and_image_metadata(self):
        docker = shutil.which("docker")
        version = subprocess.run([docker, "compose", "version"], env=RELEASE.ENV,
                                 capture_output=True, text=True, timeout=10, check=False)
        if version.returncode:
            self.skipTest("Docker Compose plugin is unavailable; no daemon is required")
        with tempfile.TemporaryDirectory(prefix="simplestchat-compose-release.") as temporary:
            root = Path(temporary).resolve()
            config, attempt = root / "config", root / "attempt"
            config.mkdir(mode=0o700)
            attempt.mkdir(mode=0o700)
            old_image = TEMPLATES.VALUES["scpub_server_image"]
            new_image = "sha256:" + "d" * 64
            for template, filename in (("public-compose.yml.j2", "compose.public.yml"),
                                       ("public-app.env.j2", "app.env"),
                                       ("public-migration.env.j2", "migration.env"),
                                       ("public-proxy.env.j2", "proxy.env")):
                (config / filename).write_text(TEMPLATES.render(template, scpub_config=str(config),
                                                               scpub_root=str(root / "data")) + "\n")
            (config / "compose.base.yml").write_bytes((PROJECT / "docker-compose.yml").read_bytes())
            runner = RELEASE.Runner(attempt)
            with patch.object(RELEASE, "CONFIG", config), patch.object(RELEASE, "DOCKER", [docker]):
                before = json.loads(runner.compose("--profile", "maintenance", "config", "--format", "json"))
                hashes_before = {service: runner.compose("config", "--hash", service)
                                 for service in ("simplestchat", "postgres", "caddy")}
                preview, environment = RELEASE.candidate_selection(runner, new_image, {"serverImage": old_image})
                (config / "compose.public.yml").write_bytes(preview)
                (config / "app.env").write_bytes(environment)
                after = json.loads(runner.compose("--profile", "maintenance", "config", "--format", "json"))
                expected = deepcopy(before)
                for service in ("simplestchat", "migrate"):
                    expected["services"][service]["image"] = new_image
                expected["services"]["simplestchat"]["environment"]["SIMPLESTCHAT_IMAGE"] = new_image
                self.assertEqual(after, expected)
                for service in ("postgres", "caddy"):
                    self.assertEqual(runner.compose("config", "--hash", service), hashes_before[service])
                self.assertNotEqual(runner.compose("config", "--hash", "simplestchat"), hashes_before["simplestchat"])
                self.assertRegex(hashes_before["simplestchat"].decode(), r"^simplestchat [a-f0-9]{64}\s*$")


if __name__ == "__main__":
    unittest.main()
