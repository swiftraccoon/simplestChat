"""Release wiring and real Compose rendering; never contact a Docker daemon."""

import hashlib
import re
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
from jinja2 import Environment, StrictUndefined, UndefinedError
from test_support import array, at, obj, objects, string, strings, yaml_value

# isort: split
import release_preflight
import release_public as RELEASE  # noqa: N812 - name the helper under test.
import test_public_templates as TEMPLATES  # noqa: N812 - shared fixture module.
from release_json import JsonObject, decode_json, json_value

# The optional Compose check renders local fixture files without daemon access.
# ruff: noqa: S603

ROOT = Path(__file__).resolve().parents[1]
PROJECT = ROOT.parents[1]


def matches(value: object, pattern: object) -> bool:
    """Expose the Ansible match predicate to the isolated Jinja environment."""
    if not isinstance(pattern, str):
        message = "Match patterns must be strings"
        raise TypeError(message)
    return re.match(pattern, str(value)) is not None


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

    @staticmethod
    def assertions_pass(task: JsonObject, values: JsonObject) -> bool:
        """Evaluate the real assertion expressions against bounded fixture values."""
        environment = Environment(undefined=StrictUndefined, autoescape=False)  # noqa: S701 - configuration.
        environment.tests["match"] = matches
        try:
            return all(
                environment.compile_expression(expression)(**values)
                for expression in strings(task, "ansible.builtin.assert", "that")
            )
        except (UndefinedError, TypeError):
            return False

    def github_values(self) -> JsonObject:
        """Provide valid fictional identities, not usable credentials or hosts."""
        return {
            "scpub_release_repository": "example/simplestChat",
            "scpub_release_artifact_id": 1234,
            "scpub_release_expected_revision": "a" * 40,
            "scpub_release_ci_run": 5678,
            "ansible_host": "chat.example.test",
            "ansible_user": "root",
            "ansible_ssh_private_key_file": "/private/controller key",
        }

    def test_artifact_sources_are_mutually_exclusive_and_select_their_own_revision(self) -> None:
        """Verify artifact sources are mutually exclusive and select their own revision."""
        expression = (
            "(scpub_release_directory is defined) != (scpub_release_artifact_id is defined)"
        )
        assertions = strings(self.play, "pre_tasks", 0, "ansible.builtin.assert", "that")
        self.assertIn(expression, assertions)
        evaluate = Environment(undefined=StrictUndefined, autoescape=False).compile_expression(  # noqa: S701 - config.
            expression
        )
        self.assertFalse(evaluate())
        self.assertTrue(evaluate(scpub_release_directory="/release"))
        self.assertTrue(evaluate(scpub_release_artifact_id=1234))
        self.assertFalse(
            evaluate(scpub_release_directory="/release", scpub_release_artifact_id=1234)
        )
        local = self.pre_task("Select the verified local source revision")
        remote = self.pre_task("Select the explicitly pinned GitHub revision")
        self.assertEqual(local["when"], "scpub_release_directory is defined")
        self.assertEqual(remote["when"], "scpub_release_artifact_id is defined")
        self.assertEqual(
            at(local, "ansible.builtin.set_fact", "scpub_release_revision"),
            "{{ (scpub_validated_release.stdout | from_json).revision }}",
        )
        self.assertEqual(
            at(remote, "ansible.builtin.set_fact", "scpub_release_revision"),
            "{{ scpub_release_expected_revision }}",
        )

    def test_github_source_requires_exact_identities_and_explicit_supported_ssh(self) -> None:
        """Verify github source requires exact identities and explicit supported ssh."""
        task = self.pre_task(
            "Require an exact GitHub source and supported key-based SSH connection"
        )
        self.assertEqual(task["when"], "scpub_release_artifact_id is defined")
        self.assertTrue(self.assertions_pass(task, self.github_values()))
        self.assertTrue(
            self.assertions_pass(
                task,
                dict(
                    self.github_values(),
                    ansible_user="deploy",
                    ansible_host="chat-alias",
                    ansible_port="2222",
                ),
            )
        )
        for key in self.github_values():
            values = self.github_values()
            del values[key]
            with self.subTest(missing=key):
                self.assertFalse(self.assertions_pass(task, values))
        for key, value in (
            ("scpub_release_repository", "https://github.com/example/repo"),
            ("scpub_release_repository", "example/repo/extra"),
            ("scpub_release_repository", "example/"),
            ("scpub_release_artifact_id", 0),
            ("scpub_release_artifact_id", True),
            ("scpub_release_artifact_id", "01"),
            ("scpub_release_ci_run", -1),
            ("scpub_release_ci_run", "1.0"),
            ("scpub_release_expected_revision", "main"),
            ("scpub_release_expected_revision", "A" * 40),
            ("ansible_connection", "local"),
            ("ansible_connection", "paramiko"),
            ("ansible_host", ""),
            ("ansible_host", "2001:db8::1"),
            ("ansible_host", "-oProxyCommand=command"),
            ("ansible_user", ""),
            ("ansible_ssh_private_key_file", "~/.ssh/id_ed25519"),
            ("ansible_port", 0),
            ("ansible_port", 65536),
            ("ansible_port", True),
            ("ansible_password", ""),
            ("ansible_ssh_pass", ""),
            ("ansible_ssh_password", ""),
            ("ansible_become_password", ""),
            ("ansible_become_pass", ""),
            ("ansible_ssh_common_args", "-J jump.example.test"),
            ("ansible_ssh_extra_args", "-F custom.conf"),
        ):
            with self.subTest(key=key, value=value):
                self.assertFalse(
                    self.assertions_pass(task, dict(self.github_values(), **{key: value}))
                )

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
            ],
        )
        self.assertEqual(fetch["delegate_to"], "localhost")
        self.assertIs(fetch["become"], expr2=False)
        self.assertEqual(fetch["timeout"], 450)
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

    def test_stage_is_default_and_deployment_is_an_explicit_separate_job(self) -> None:
        """Verify stage is default and deployment is an explicit separate job."""
        stage = self.command("stage")
        deploy = self.command("deploy")
        self.assertLess(self.tasks.index(stage), self.tasks.index(deploy))
        self.assertEqual(stage["when"], "not ansible_check_mode")
        self.assertEqual(
            deploy["when"],
            [
                "not ansible_check_mode",
                "scpub_release_deploy | default(false) | bool",
            ],
        )
        for action, task, runtime, grace in (
            ("stage", stage, 600, 60),
            # The deploy bound covers the quiet wait for empty rooms (at most 600 s)
            # plus the replacement itself.
            ("deploy", deploy, 1800, 240),
        ):
            argv = strings(task, "ansible.builtin.command", "argv")
            self.assertEqual(argv[0], "systemd-run")
            self.assertIn("--wait", argv)
            self.assertIn(f"--property=RuntimeMaxSec={runtime}", argv)
            self.assertIn(f"--property=TimeoutStopSec={grace}", argv)
            tail = [
                "/usr/bin/python3",
                "-B",
                "/usr/local/libexec/simplestchat-public/release-public.py",
                action,
                "{{ scpub_release_revision }}",
            ]
            if action == "deploy":
                tail += ["--quiet-seconds", "{{ scpub_release_quiet_seconds | default(600) | int }}"]
            self.assertEqual(argv[-len(tail) :], tail)
        self.assertEqual(self.play["serial"], 1)
        for task in (stage, deploy):
            self.assertEqual(at(task, "vars", "ansible_python_interpreter"), "/usr/bin/python3")
            unit = string(task, "ansible.builtin.command", "argv", 1)
            self.assertIn("(scpub_release_preflight.stdout | from_json).runId", unit)
            self.assertNotIn("ansible_facts", unit)

    def test_routine_release_has_no_host_maintenance_or_configuration_rendering(self) -> None:
        """Verify routine release has no host maintenance or configuration rendering."""
        allowed = {
            "ansible.builtin.assert",
            "ansible.builtin.command",
            "ansible.builtin.copy",
            "ansible.builtin.file",
            "ansible.builtin.set_fact",
            "ansible.builtin.stat",
        }
        for task in objects(self.play, "pre_tasks") + self.tasks:
            modules = [key for key in task if key.startswith("ansible.builtin.")]
            self.assertEqual(len(modules), 1)
            self.assertIn(modules[0], allowed)
            argv = array(obj(task.get("ansible.builtin.command", {})).get("argv", []))
            self.assertTrue(
                {"apt", "reboot", "stop", "restart", "up", "down", "build", "pull"}.isdisjoint(argv)
            )
        assertions = strings(self.play, "pre_tasks", 0, "ansible.builtin.assert", "that")
        self.assertIn("scpub_enabled | bool", assertions)
        local_source = self.pre_task("Require an absolute local artifact directory")
        self.assertEqual(local_source["when"], "scpub_release_directory is defined")
        self.assertIn(
            "scpub_release_directory is match('^/')",
            strings(local_source, "ansible.builtin.assert", "that"),
        )
        self.assertIn("scpub_config == '/etc/simplestchat-public'", assertions)
        self.assertIn("scpub_root == '/srv/simplestchat-public'", assertions)
        self.assertIs(self.play["gather_facts"], expr2=False)
        preflight = self.pre_task(
            "Verify the host and exact prepared helpers without broad fact gathering"
        )
        self.assertIs(preflight["check_mode"], expr2=False)
        self.assertIs(preflight["changed_when"], expr2=False)
        self.assertEqual(preflight["timeout"], 60)
        self.assertEqual(at(preflight, "vars", "ansible_python_interpreter"), "/usr/bin/python3")
        self.assertEqual(
            strings(preflight, "ansible.builtin.command", "argv")[:3],
            ["/usr/bin/python3", "-B", "-c"],
        )

    def test_no_overwrite_is_paired_with_exact_controller_and_destination_identity(self) -> None:
        """Verify no overwrite is paired with exact controller and destination identity."""
        validation = next(
            task
            for task in objects(self.play, "pre_tasks")
            if task.get("register") == "scpub_validated_release"
        )
        self.assertEqual(validation["delegate_to"], "localhost")
        self.assertIs(validation["become"], expr2=False)
        self.assertIs(validation["changed_when"], expr2=False)
        self.assertEqual(validation["when"], "scpub_release_directory is defined")
        code = string(validation, "ansible.builtin.command", "argv", 3)
        self.assertIn("validate_manifest(root / 'release.json')", code)
        self.assertIn("verify_archive(root / 'image.tar', manifest)", code)
        self.assertIn("'image.tar': manifest['archiveSha256']", code)
        self.assertIn("'release.json': sha256_file(root / 'release.json')", code)
        transfer = next(
            task
            for task in self.tasks
            if task.get("loop") == ["release.json", "image.tar"] and "ansible.builtin.copy" in task
        )
        copy = obj(transfer, "ansible.builtin.copy")
        self.assertIs(copy["force"], expr2=False)
        self.assertEqual(
            copy["dest"], "{{ scpub_root }}/releases/{{ scpub_release_revision }}/{{ item }}"
        )
        self.assertEqual((copy["owner"], copy["group"], copy["mode"]), ("root", "root", "0600"))
        inspection = next(
            task for task in self.tasks if task.get("register") == "scpub_transferred_files"
        )
        self.assertEqual(at(inspection, "ansible.builtin.stat", "checksum_algorithm"), "sha256")
        self.assertIs(at(inspection, "ansible.builtin.stat", "follow"), expr2=False)
        comparison = next(task for task in self.tasks if "ansible.builtin.assert" in task)
        self.assertEqual(comparison["loop"], "{{ scpub_transferred_files.results | default([]) }}")
        assertions = strings(comparison, "ansible.builtin.assert", "that")
        for assertion in [
            "item.stat.isreg | default(false)",
            "item.stat.uid == 0",
            "item.stat.mode == '0600'",
            "item.stat.checksum == (scpub_validated_release.stdout | from_json)[item.item]",
        ]:
            self.assertIn(assertion, assertions)
        self.assertLess(self.tasks.index(transfer), self.tasks.index(inspection))
        self.assertLess(self.tasks.index(inspection), self.tasks.index(comparison))
        self.assertLess(self.tasks.index(comparison), self.tasks.index(self.command("stage")))
        for task in (transfer, inspection, comparison):
            self.assertEqual(task["when"], "scpub_release_directory is defined")

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

    def test_prepared_mode_skips_helper_writes_but_keeps_local_release_storage(self) -> None:
        """Verify prepared mode skips helper writes but keeps local release storage."""
        for task in self.tasks:
            if string(task, "name").startswith("Install "):
                condition = task["when"]
                if isinstance(condition, list):
                    self.assertIn("not (scpub_release_prepared | default(false) | bool)", condition)
                else:
                    self.assertEqual(
                        condition, "not (scpub_release_prepared | default(false) | bool)"
                    )
        storage = next(
            task
            for task in self.tasks
            if task["name"] == "Create private immutable release storage"
        )
        self.assertEqual(
            storage["when"],
            "not (scpub_release_prepared | default(false) | bool) "
            + "or scpub_release_directory is defined",
        )
        self.assertIs(at(self.play, "vars", "ansible_pipelining"), expr2=True)
        # Do not override the delegated controller's Python with Debian's path.
        self.assertNotIn("ansible_python_interpreter", obj(self.play, "vars"))

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
            runner = RELEASE.Runner(attempt)
            with patch.object(RELEASE, "CONFIG", config), patch.object(RELEASE, "DOCKER", [docker]):
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
