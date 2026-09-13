"""Offline automation checks; never connect to a host or start Docker."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

from jinja2 import Environment, StrictUndefined
import yaml

ROOT = Path(__file__).resolve().parents[1]
REVISION = "a" * 40


def render(name):
    values = yaml.safe_load((ROOT / "group_vars/benchmark_hosts.yml").read_text())
    values["scbench_revision"] = REVISION
    values["scbench_release_dir"] = f"{values['scbench_root']}/sources/{REVISION}"
    return Environment(undefined=StrictUndefined).from_string(
        (ROOT / "templates" / name).read_text()
    ).render(values)


class AutomationTests(unittest.TestCase):
    def test_shell_templates_parse_and_pass_shellcheck(self):
        for name in ["build-images.sh.j2", "run-benchmark.sh.j2"]:
            script = render(name)
            self.assertNotIn("{{ scbench_", script)
            for argv in [["bash", "-n"], ["shellcheck", "--shell=bash", "-"]]:
                result = subprocess.run(argv, input=script, text=True, capture_output=True, timeout=15)
                self.assertEqual(result.returncode, 0, f"{name}: {result.stdout}{result.stderr}")

    def test_inventory_host_values_override_safe_defaults(self):
        with tempfile.TemporaryDirectory(prefix="simplestchat-ansible-test.") as directory:
            inventory = Path(directory) / "inventory.yml"
            inventory.write_text(yaml.safe_dump({"benchmark_hosts": {"hosts": {"fixture": {
                "ansible_connection": "local", "scbench_revision": REVISION,
                "scbench_upgrade_packages": True, "scbench_reboot": True,
                "scbench_workload": {"clients": 8, "workers": 2},
            }}}}))
            result = subprocess.run(
                ["ansible-inventory", "--playbook-dir", str(ROOT), "-i", str(inventory), "--host", "fixture"],
                text=True, capture_output=True, timeout=15,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            values = json.loads(result.stdout)
            self.assertTrue(values["scbench_upgrade_packages"])
            self.assertTrue(values["scbench_reboot"])
            self.assertEqual(values["scbench_workload"]["workers"], 2)
        defaults = yaml.safe_load((ROOT / "group_vars/benchmark_hosts.yml").read_text())
        self.assertFalse(defaults["scbench_upgrade_packages"])
        self.assertFalse(defaults["scbench_reboot"])

    def test_tagged_runs_cannot_skip_safety_preflight(self):
        result = subprocess.run(
            ["ansible-playbook", "-i", str(ROOT / "inventory.example.yml"),
             str(ROOT / "site.yml"), "--list-tasks", "--tags", "benchmark"],
            text=True, capture_output=True, timeout=15,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        for guard in [
            "Require the supported host", "Refuse provisioning during an active benchmark",
            "Refuse provisioning during an active image build", "Refuse provisioning while container cleanup remains uncertain",
        ]:
            self.assertIn(guard, result.stdout)
        self.assertLess(result.stdout.index("Refuse provisioning"), result.stdout.index("Install the owned container lifecycle helpers"))

    def test_services_are_explicit_and_preserve_owned_cleanup(self):
        benchmark = render("benchmark.service.j2")
        builder = render("image-build.service.j2")
        for unit in [benchmark, builder]:
            self.assertNotIn("[Install]", unit)
            self.assertNotIn("Restart=always", unit)
            self.assertNotIn("Conflicts=", unit, "Starting a build must not stop a running benchmark")
        self.assertIn("ExecStopPost=/usr/local/libexec/simplestchat-bench/cleanup-container-benchmark.sh", benchmark)
        self.assertIn("RuntimeMaxSec=900", benchmark)
        self.assertIn("RuntimeDirectoryPreserve=yes", benchmark)
        script = render("build-images.sh.j2")
        self.assertIn("/run/simplestchat-bench/workload.lock", script)
        self.assertIn(".finalized == true", script)
        self.assertNotIn("docker run", script, "Image verification must not leave daemon-owned processes")
        self.assertIn('ln "$attempt/images.json" "$image_manifest"', script)
        self.assertLess(
            script.index('verify_images "$attempt/images.json"'),
            script.index('ln "$attempt/images.json" "$image_manifest"'),
            "A failed image verification must not publish the stable success manifest",
        )

    def test_package_preferences_cover_only_the_five_selected_versions(self):
        preferences = render("docker.preferences.j2")
        records = [block for block in preferences.split("\n\n") if "Package:" in block]
        expected = {
            "docker-ce": "5:29.8.0-1~debian.13~trixie",
            "docker-ce-cli": "5:29.8.0-1~debian.13~trixie",
            "containerd.io": "2.3.5-1~debian.13~trixie",
            "docker-buildx-plugin": "0.37.1-1~debian.13~trixie",
            "docker-compose-plugin": "5.5.1-1~debian.13~trixie",
        }
        self.assertEqual(len(records), len(expected))
        for package, version in expected.items():
            self.assertIn(f"Package: {package}\nPin: version {version}\nPin-Priority: 1000", preferences)
        tasks = (ROOT / "tasks/docker.yml").read_text()
        self.assertLess(tasks.index("docker.preferences.j2"), tasks.index("docker-ce={{"))
        self.assertIn("allow_downgrade: false", tasks)

    def test_image_build_refuses_a_running_public_project_before_image_work(self):
        script = render("build-images.sh.j2")
        start = script.index("running_public=$(timeout --signal=TERM --kill-after=1s 8s docker ps --quiet")
        end = script.index('cd "$source_root"')
        guard = script[start:end]
        self.assertIn("--filter 'label=com.docker.compose.project=simplestchat-public'", guard)
        self.assertIn("Cannot verify whether public chat is running", guard)
        self.assertIn('[[ -z "$running_public" ]] || {', guard)
        self.assertIn("Public chat is running; stop it explicitly", guard)
        self.assertEqual(guard.count("exit 1;"), 2, "Both unknown and running public states must fail closed")
        self.assertEqual(guard.count("docker "), 1, "The public preflight may only inspect containers")
        self.assertLess(script.index("flock -n 9"), start)
        self.assertLess(script.index(".finalized == true"), start)
        self.assertLess(end, script.index("verify_images()"))
        self.assertLess(end, script.index("docker build --pull"))

    def test_static_units_are_inspected_without_repeated_disable_changes(self):
        tasks = yaml.safe_load((ROOT / "tasks/benchmark.yml").read_text())
        inspection = next(task for task in tasks if task.get("register") == "scbench_unit_enablement")
        guard = next(task for task in tasks if "ansible.builtin.assert" in task)
        disable = next(task for task in tasks if "ansible.builtin.systemd_service" in task)
        reload_units = next(task for task in tasks if task.get("ansible.builtin.meta") == "flush_handlers")
        self.assertEqual(inspection["ansible.builtin.command"]["argv"], [
            "/usr/bin/systemctl", "is-enabled", "simplestchat-{{ item }}.service",
        ])
        self.assertEqual(inspection["loop"], ["image-build", "benchmark"])
        self.assertIs(inspection["changed_when"], False)
        self.assertIs(inspection["check_mode"], False)
        self.assertLess(tasks.index(reload_units), tasks.index(inspection))
        self.assertLess(tasks.index(inspection), tasks.index(guard))
        self.assertLess(tasks.index(guard), tasks.index(disable))
        self.assertEqual(guard["loop"], "{{ scbench_unit_enablement.results }}")
        self.assertEqual(disable["loop"], guard["loop"])
        self.assertEqual(disable["ansible.builtin.systemd_service"], {
            "name": "simplestchat-{{ item.item }}.service", "enabled": False,
        })
        self.assertNotIn("changed_when", disable, "An actual disable must keep normal change reporting")

        environment = Environment(undefined=StrictUndefined)
        accepted = [environment.compile_expression(value) for value in guard["ansible.builtin.assert"]["that"]]
        should_disable = environment.compile_expression(disable["when"])
        inspection_failed = environment.compile_expression(inspection["failed_when"])
        for state, rc, needs_change in [
            ("enabled", 0, True), ("enabled-runtime", 0, True),
            ("disabled", 1, False), ("static", 0, False),
        ]:
            with self.subTest(state=state):
                result = {"stdout": f"{state}\n", "rc": rc, "item": "benchmark"}
                self.assertFalse(inspection_failed(scbench_unit_enablement=result))
                self.assertTrue(all(check(item=result) for check in accepted))
                self.assertEqual(should_disable(item=result), needs_change)
        for state in ["masked", "masked-runtime", "linked", "linked-runtime", "indirect", "generated", "transient", "not-found", ""]:
            with self.subTest(unexpected_state=state):
                result = {"stdout": state, "rc": 1, "item": "benchmark"}
                self.assertFalse(all(check(item=result) for check in accepted))
                self.assertFalse(should_disable(item=result))
        self.assertTrue(inspection_failed(scbench_unit_enablement={"rc": 4}))


if __name__ == "__main__":
    os.environ["ANSIBLE_CONFIG"] = str(ROOT / "ansible.cfg")
    unittest.main()
