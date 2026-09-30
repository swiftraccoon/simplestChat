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

    def test_only_current_signed_github_artifacts_can_select_a_release(self) -> None:
        """An unsigned local directory cannot bypass the required source/run contract."""
        assertions = strings(self.play, "pre_tasks", 0, "ansible.builtin.assert", "that")
        self.assertIn("scpub_release_directory is not defined", assertions)
        self.assertIn("scpub_release_artifact_id is defined", assertions)
        remote = self.pre_task("Select the explicitly pinned GitHub revision")
        self.assertEqual(
            at(remote, "ansible.builtin.set_fact", "scpub_release_revision"),
            "{{ scpub_release_expected_revision }}",
        )
        self.assertFalse(
            any(
                task.get("name") == "Select the verified local source revision"
                for task in objects(self.play, "pre_tasks")
            )
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
                tail += [
                    "--quiet-seconds",
                    "{{ scpub_release_quiet_seconds | default(600) | int }}",
                ]
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

    def test_attestation_precedes_every_remote_preflight_and_cached_bytes_are_reverified(
        self,
    ) -> None:
        """Fresh cryptographic verification is mandatory even in prepared and check modes."""
        validation = self.pre_task(
            "Verify the signed release on the controller before any remote preflight"
        )
        self.assertEqual(validation["delegate_to"], "localhost")
        self.assertIs(validation["become"], expr2=False)
        self.assertIs(validation["changed_when"], expr2=False)
        self.assertIs(validation["check_mode"], expr2=False)
        self.assertNotIn("when", validation)
        argv = strings(validation, "ansible.builtin.command", "argv")
        self.assertIn("{{ playbook_dir }}/../../build/verify-release.py", argv)
        self.assertIn("--artifact-dir", argv)
        self.assertIn("--ci-run", argv)
        preflight = self.pre_task(
            "Verify the host and exact prepared helpers without broad fact gathering"
        )
        pre_tasks = objects(self.play, "pre_tasks")
        self.assertLess(pre_tasks.index(validation), pre_tasks.index(preflight))
        fetch = self.command("--artifact-id")
        fetch_argv = strings(fetch, "ansible.builtin.command", "argv")
        self.assertEqual(
            fetch_argv[-2:],
            [
                "--verified-directory",
                "{{ (scpub_validated_release.stdout | from_json).directory }}",
            ],
        )
        self.assertLess(self.tasks.index(fetch), self.tasks.index(self.command("stage")))
        self.assertFalse(
            any(task.get("loop") == ["release.json", "image.tar"] for task in self.tasks)
        )

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
            "not (scpub_release_prepared | default(false) | bool)",
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


class MaintenancePlaybookTests(unittest.TestCase):
    """The maintenance playbook checks out the source, renders grants and runs the launcher."""

    @cached_property
    def play(self) -> JsonObject:
        """Read the maintenance playbook once per isolated test case."""
        return obj(yaml_value((ROOT / "maintenance.yml").read_text()), 0)

    def test_source_candidate_env_grants_and_launcher_run_as_one_transient_unit(self) -> None:
        """Verify source, the candidate app.env, grants and launcher run in that order."""
        tasks = objects(self.play, "tasks")
        self.assertEqual(self.play["hosts"], "benchmark_hosts")
        self.assertEqual(self.play["serial"], 1)
        # The candidate app.env is sized from the host's facts.
        self.assertIs(self.play["gather_facts"], expr2=True)
        self.assertEqual(at(self.play, "vars", "scbench_revision"), "{{ scpub_release_revision }}")
        assertions = strings(self.play, "pre_tasks", 0, "ansible.builtin.assert", "that")
        self.assertIn("scpub_release_revision is match('^[a-f0-9]{40}$')", assertions)
        self.assertIn("scpub_enabled | bool", assertions)
        self.assertEqual(tasks[0]["ansible.builtin.import_tasks"], "tasks/source.yml")
        templates = [
            obj(task["ansible.builtin.template"])
            for task in tasks
            if "ansible.builtin.template" in task
        ]
        self.assertEqual(
            [(template["src"], template["dest"]) for template in templates],
            [
                ("public-app.env.j2", "{{ scpub_config }}/app.env.candidate"),
                ("public-runtime-grants.sql.j2", "{{ scpub_config }}/runtime-grants.sql"),
            ],
        )
        self.assertTrue(all(template["mode"] == "0600" for template in templates))
        candidate = next(task for task in tasks if "ansible.builtin.template" in task)
        self.assertIs(candidate["no_log"], expr2=True)
        # The template needs the secrets, the selected image and, when enabled, the relay secret.
        facts = [
            obj(task["ansible.builtin.set_fact"])
            for task in tasks
            if "ansible.builtin.set_fact" in task
        ]
        self.assertEqual(
            facts[0]["scpub_server_image"],
            "{{ (scpub_selected_images.content | b64decode | from_json).serverImage }}",
        )
        self.assertIn("scpub_secrets", facts[0])
        self.assertIn("scpub_turn_secret", facts[1])
        slurps = [
            at(task, "ansible.builtin.slurp", "src")
            for task in tasks
            if "ansible.builtin.slurp" in task
        ]
        self.assertEqual(
            slurps,
            [
                "{{ scpub_config }}/images.json",
                "{{ scpub_config }}/secrets.json",
                "/etc/simplestchat-turn/secret",
            ],
        )
        for task in tasks:
            if "ansible.builtin.slurp" in task and "secret" in str(task["ansible.builtin.slurp"]):
                self.assertIs(task["no_log"], expr2=True)
        launcher = tasks[-1]
        argv = strings(launcher, "ansible.builtin.command", "argv")
        self.assertEqual(argv[0], "systemd-run")
        self.assertIn("--property=RuntimeMaxSec=1800", argv)
        self.assertIn("--property=TimeoutStopSec=240", argv)
        self.assertIn("--wait", argv)
        self.assertEqual(
            argv[-7:],
            [
                "/usr/bin/python3",
                "-B",
                "/usr/local/libexec/simplestchat-public/release-public.py",
                "maintain",
                "{{ scpub_release_revision }}",
                "--candidate-env",
                "{{ scpub_config }}/app.env.candidate",
            ],
        )
        self.assertTrue(argv[1].startswith("--unit=simplestchat-maintenance-"))
        self.assertEqual(launcher["when"], "not ansible_check_mode")
        self.assertEqual(at(launcher, "vars", "ansible_python_interpreter"), "/usr/bin/python3")
        source = (ROOT / "tasks/source.yml").read_text()
        self.assertIn("force: false", source)


class PublicPlaybookSelectionTests(unittest.TestCase):
    """The full playbook takes the image from an on-host build or a staged release."""

    def test_a_staged_release_replaces_the_on_host_build_as_the_image_source(self) -> None:
        """With scpub_release_revision the staged image is selected and its label checked."""
        play = obj(yaml_value((ROOT / "public.yml").read_text()), 0)
        assertions = strings(play, "pre_tasks", 0, "ansible.builtin.assert", "that")
        self.assertIn(
            "scpub_release_revision is not defined or scpub_release_revision == scbench_revision",
            assertions,
        )
        tasks = {string(task, "name"): task for task in objects(play, "pre_tasks")}
        for name in (
            "Read the previously verified application image manifest",
            "Select the retained application artifact without rebuilding",
        ):
            self.assertEqual(tasks[name]["when"], "scpub_release_revision is not defined")
        staged = tasks["Read the release the controller staged while chat was live"]
        self.assertEqual(staged["when"], "scpub_release_revision is defined")
        self.assertEqual(
            at(staged, "ansible.builtin.slurp", "src"),
            "{{ scpub_root }}/releases/{{ scpub_release_revision }}/staged.json",
        )
        label = tasks["Require the staged image to carry the release revision"]
        self.assertEqual(label["when"], "scpub_release_revision is defined")
        self.assertIn(
            "org.opencontainers.image.revision",
            " ".join(strings(label, "ansible.builtin.command", "argv")),
        )
        self.assertIn("scpub_release_revision", string(label, "failed_when"))
        selection = tasks["Retain the verified application image selection"]
        self.assertIn(
            "scpub_staged_release",
            string(selection, "ansible.builtin.set_fact", "scpub_server_image"),
        )
        self.assertEqual(
            at(
                tasks["Require a content-addressed application image"],
                "ansible.builtin.assert",
                "that",
            ),
            "scpub_server_image is match('^sha256:[a-f0-9]{64}$')",
        )
