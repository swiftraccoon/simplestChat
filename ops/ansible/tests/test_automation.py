"""Offline automation checks; never connect to a host or start Docker."""

import io
import json
import os
import re
import stat
import subprocess
import tempfile
import unittest
from contextlib import redirect_stdout
from dataclasses import dataclass
from pathlib import Path
from typing import cast
from unittest.mock import patch

import yaml
from jinja2 import Environment, StrictUndefined
from jinja2.environment import TemplateExpression
from test_support import at, obj, objects, string, strings, yaml_value

# isort: split
from release_json import JsonObject, boolean_value, decode_json, integer_value

# These tests invoke reviewed checkout syntax tools and execute the checked-in
# guard only with isolated filesystem doubles, never shell input or remote code.
# ruff: noqa: S603, S607, S102

ROOT = Path(__file__).resolve().parents[1]
REVISION = "a" * 40


def render(name: str) -> str:
    """Render a benchmark configuration with fixed, non-production fixture values."""
    values = obj(yaml_value((ROOT / "group_vars/benchmark_hosts.yml").read_text()))
    values["scbench_revision"] = REVISION
    values["scbench_release_dir"] = f"{values['scbench_root']}/sources/{REVISION}"
    return (
        Environment(undefined=StrictUndefined, autoescape=False)  # noqa: S701 - config, not HTML.
        .from_string((ROOT / "templates" / name).read_text())
        .render(values)
    )


def embedded_guard(script: str) -> str:
    """Require and extract the exact inline release-journal guard under test."""
    match = re.search(r"python3 - <<'PY'\n(.*?)\nPY", script, re.DOTALL)
    if match is None:
        message = "Missing release-journal guard"
        raise AssertionError(message)
    return match[1]


def evaluate(expression: TemplateExpression, **values: JsonObject) -> bool:
    """Evaluate an Ansible condition and require a real boolean result."""
    result = cast("object", expression(**values))
    if not isinstance(result, bool):
        message = "Configuration condition did not evaluate to a boolean"
        raise AssertionError(message)  # noqa: TRY004 - wrong fixture types are test failures.
    return result


@dataclass(frozen=True, slots=True, kw_only=True)
class Metadata:
    """Filesystem fields used by the journal guard."""

    st_uid: int = 0
    st_mode: int = stat.S_IFREG | 0o600
    st_size: int = 100


@dataclass(frozen=True, slots=True, kw_only=True)
class Parent:
    """Minimal parent-directory metadata double."""

    metadata: Metadata

    def lstat(self) -> Metadata:
        """Return configured metadata without contacting the filesystem."""
        return self.metadata


@dataclass(frozen=True, slots=True, kw_only=True)
class JournalPath:
    """Typed journal fixture; no methods consult an actual journal path."""

    metadata: Metadata
    parent: Parent
    present: bool
    symlink: bool
    text: str

    def exists(self) -> bool:
        """Report the modeled file presence."""
        return self.present

    def is_symlink(self) -> bool:
        """Report whether this fixture models an unsafe symlink."""
        return self.symlink

    def lstat(self) -> Metadata:
        """Return modeled owner, mode and length."""
        return self.metadata

    def read_text(self) -> str:
        """Return the fixture journal, without reading production files."""
        return self.text


class AutomationTests(unittest.TestCase):
    """Verify the automation contract offline."""

    def test_shell_templates_parse_and_pass_shellcheck(self) -> None:
        """Verify shell templates parse and pass shellcheck."""
        for name in ["build-images.sh.j2", "run-benchmark.sh.j2"]:
            script = render(name)
            self.assertNotIn("{{ scbench_", script)
            for argv in [["bash", "-n"], ["shellcheck", "--shell=bash", "-"]]:
                result = subprocess.run(
                    argv, input=script, text=True, capture_output=True, timeout=15, check=False
                )
                self.assertEqual(result.returncode, 0, f"{name}: {result.stdout}{result.stderr}")

    def test_inventory_host_values_override_safe_defaults(self) -> None:
        """Verify inventory host values override safe defaults."""
        with tempfile.TemporaryDirectory(prefix="simplestchat-ansible-test.") as directory:
            inventory = Path(directory) / "inventory.yml"
            _ = inventory.write_text(
                yaml.safe_dump(
                    {
                        "benchmark_hosts": {
                            "hosts": {
                                "fixture": {
                                    "ansible_connection": "local",
                                    "scbench_revision": REVISION,
                                    "scbench_upgrade_packages": True,
                                    "scbench_reboot": True,
                                    "scbench_workload": {"clients": 8, "workers": 2},
                                }
                            }
                        }
                    }
                )
            )
            result = subprocess.run(
                [
                    "ansible-inventory",
                    "--playbook-dir",
                    str(ROOT),
                    "-i",
                    str(inventory),
                    "--host",
                    "fixture",
                ],
                text=True,
                capture_output=True,
                timeout=15,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            values = obj(decode_json(result.stdout))
            self.assertTrue(values["scbench_upgrade_packages"])
            self.assertTrue(values["scbench_reboot"])
            self.assertEqual(at(values, "scbench_workload", "workers"), 2)
        defaults = obj(yaml_value((ROOT / "group_vars/benchmark_hosts.yml").read_text()))
        self.assertFalse(defaults["scbench_upgrade_packages"])
        self.assertFalse(defaults["scbench_reboot"])

    def test_tagged_runs_cannot_skip_safety_preflight(self) -> None:
        """Verify tagged runs cannot skip safety preflight."""
        result = subprocess.run(
            [
                "ansible-playbook",
                "-i",
                str(ROOT / "inventory.example.yml"),
                str(ROOT / "site.yml"),
                "--list-tasks",
                "--tags",
                "benchmark",
            ],
            text=True,
            capture_output=True,
            timeout=15,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        for guard in [
            "Require the supported host",
            "Refuse provisioning during an active benchmark",
            "Refuse provisioning during an active image build",
            "Refuse provisioning while container cleanup remains uncertain",
            "Refuse provisioning while a public release remains unfinished",
        ]:
            self.assertIn(guard, result.stdout)
        self.assertLess(
            result.stdout.index("Refuse provisioning"),
            result.stdout.index("Install the owned container lifecycle helpers"),
        )

    def test_services_are_explicit_and_preserve_owned_cleanup(self) -> None:
        """Verify services are explicit and preserve owned cleanup."""
        benchmark = render("benchmark.service.j2")
        builder = render("image-build.service.j2")
        for unit in [benchmark, builder]:
            self.assertNotIn("[Install]", unit)
            self.assertNotIn("Restart=always", unit)
            self.assertNotIn(
                "Conflicts=", unit, "Starting a build must not stop a running benchmark"
            )
        self.assertIn(
            "ExecStopPost=/usr/local/libexec/simplestchat-bench/cleanup-container-benchmark.sh",
            benchmark,
        )
        self.assertIn("RuntimeMaxSec=900", benchmark)
        self.assertIn("RuntimeDirectoryPreserve=yes", benchmark)
        script = render("build-images.sh.j2")
        self.assertIn("/run/simplestchat-bench/workload.lock", script)
        self.assertIn(".finalized == true", script)
        self.assertNotIn(
            "docker run", script, "Image verification must not leave daemon-owned processes"
        )
        self.assertIn('ln "$attempt/images.json" "$image_manifest"', script)
        self.assertLess(
            script.index('verify_images "$attempt/images.json"'),
            script.index('ln "$attempt/images.json" "$image_manifest"'),
            "A failed image verification must not publish the stable success manifest",
        )

    def test_package_preferences_cover_only_the_five_selected_versions(self) -> None:
        """Verify package preferences cover only the five selected versions."""
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
            self.assertIn(
                f"Package: {package}\nPin: version {version}\nPin-Priority: 1000", preferences
            )
        tasks = (ROOT / "tasks/docker.yml").read_text()
        self.assertLess(tasks.index("docker.preferences.j2"), tasks.index("docker-ce={{"))
        self.assertIn("allow_downgrade: false", tasks)

    def test_image_build_refuses_a_running_public_project_before_image_work(self) -> None:
        """Verify image build refuses a running public project before image work."""
        script = render("build-images.sh.j2")
        start = script.index(
            "running_public=$(timeout --signal=TERM --kill-after=1s 8s docker ps --quiet"
        )
        end = script.index('cd "$source_root"')
        guard = script[start:end]
        self.assertIn("--filter 'label=com.docker.compose.project=simplestchat-public'", guard)
        self.assertIn("Cannot verify whether public chat is running", guard)
        self.assertIn('[[ -z "$running_public" ]] || {', guard)
        self.assertIn("Public chat is running; stop it explicitly", guard)
        self.assertEqual(
            guard.count("exit 1;"), 2, "Both unknown and running public states must fail closed"
        )
        self.assertEqual(
            guard.count("docker "), 1, "The public preflight may only inspect containers"
        )
        self.assertLess(script.index("flock -n 9"), start)
        self.assertLess(script.index(".finalized == true"), start)
        self.assertLess(end, script.index("verify_images()"))
        self.assertLess(end, script.index("docker build --pull"))

    def test_persistent_release_guards_are_identical_and_precede_work(self) -> None:
        """Verify persistent release guards are identical and precede work."""
        scripts = [render(name) for name in ("build-images.sh.j2", "run-benchmark.sh.j2")]
        scripts.append((ROOT.parents[1] / "build/benchmark-container.sh").read_text())
        scripts.append((ROOT / "files/deploy-public.sh").read_text())
        guards = [embedded_guard(script) for script in scripts]
        for filename in ("site.yml", "public.yml"):
            play = obj(yaml_value((ROOT / filename).read_text()), 0)
            task = next(
                task
                for task in objects(play, "pre_tasks")
                if "release-state.json" in str(task.get("ansible.builtin.command", {}))
            )
            self.assertEqual(task["tags"], ["always"])
            self.assertIs(task["changed_when"], expr2=False)
            self.assertIs(task["check_mode"], expr2=False)
            guards.append(string(task, "ansible.builtin.command", "argv", -1).rstrip())
        self.assertTrue(all(guard == guards[0] for guard in guards))
        for script, before, after in [
            (scripts[0], "flock -n 9", "running_public=$("),
            (scripts[2], "flock --exclusive --nonblock 9", "endpoint="),
            (scripts[3], "flock -n 9", "compose()"),
        ]:
            self.assertLess(script.index(before), script.index(guards[0]))
            self.assertLess(script.index(guards[0]), script.index(after))
        self.assertLess(scripts[1].index(guards[0]), scripts[1].index("manifest="))

    def test_persistent_release_guard_rejects_unfinished_or_unprotected_records(self) -> None:
        """Verify persistent release guard rejects unfinished or unprotected records."""
        guard = embedded_guard(render("run-benchmark.sh.j2"))
        completed = {"schemaVersion": 1, "finalized": True}
        cases: list[tuple[JsonObject, bool]] = [
            ({}, True),
            ({"exists": False}, True),
            ({"value": dict(completed, finalized=False)}, False),
            ({"value": dict(completed, finalized=1)}, False),
            ({"value": dict(completed, finalized="true")}, False),
            ({"value": dict(completed, schemaVersion=True)}, False),
            ({"value": dict(completed, schemaVersion=2)}, False),
            ({"value": []}, False),
            ({"value": {}}, False),
            ({"text": "invalid JSON"}, False),
            ({"uid": 501}, False),
            ({"mode": stat.S_IFREG | 0o644}, False),
            ({"mode": stat.S_IFDIR | 0o600}, False),
            ({"mode": stat.S_IFIFO | 0o600}, False),
            ({"mode": stat.S_IFLNK | 0o600}, False),
            ({"exists": False, "symlink": True, "mode": stat.S_IFLNK | 0o600}, False),
            ({"size": 0}, False),
            ({"size": 16385}, False),
            ({"parent_uid": 501}, False),
            ({"parent_mode": stat.S_IFDIR | 0o755}, False),
            ({"parent_mode": stat.S_IFLNK | 0o700}, False),
        ]
        for changes, permitted in cases:
            with self.subTest(changes=changes):
                metadata = Metadata(
                    st_uid=integer_value(changes.get("uid", 0)),
                    st_mode=integer_value(changes.get("mode", stat.S_IFREG | 0o600)),
                    st_size=integer_value(changes.get("size", 100)),
                )
                parent = Parent(
                    metadata=Metadata(
                        st_uid=integer_value(changes.get("parent_uid", 0)),
                        st_mode=integer_value(changes.get("parent_mode", stat.S_IFDIR | 0o700)),
                    )
                )
                record = JournalPath(
                    present=boolean_value(changes.get("exists", True)),
                    symlink=boolean_value(changes.get("symlink", False)),
                    metadata=metadata,
                    parent=parent,
                    text=string(changes.get("text", json.dumps(changes.get("value", completed)))),
                )
                with (
                    patch("pathlib.Path", return_value=record) as factory,
                    redirect_stdout(io.StringIO()),
                ):
                    if permitted:
                        exec(compile(guard, "<release-guard>", "exec"), {})
                    else:
                        with self.assertRaises((AssertionError, ValueError)):
                            exec(compile(guard, "<release-guard>", "exec"), {})
                    factory.assert_called_once_with("/srv/simplestchat-public/release-state.json")

    def test_static_units_are_inspected_without_repeated_disable_changes(self) -> None:
        """Verify static units are inspected without repeated disable changes."""
        tasks = objects(yaml_value((ROOT / "tasks/benchmark.yml").read_text()))
        inspection = next(
            task for task in tasks if task.get("register") == "scbench_unit_enablement"
        )
        guard = next(task for task in tasks if "ansible.builtin.assert" in task)
        disable = next(task for task in tasks if "ansible.builtin.systemd_service" in task)
        reload_units = next(
            task for task in tasks if task.get("ansible.builtin.meta") == "flush_handlers"
        )
        self.assertEqual(
            at(inspection, "ansible.builtin.command", "argv"),
            [
                "/usr/bin/systemctl",
                "is-enabled",
                "simplestchat-{{ item }}.service",
            ],
        )
        self.assertEqual(inspection["loop"], ["image-build", "benchmark"])
        self.assertIs(inspection["changed_when"], expr2=False)
        self.assertIs(inspection["check_mode"], expr2=False)
        self.assertLess(tasks.index(reload_units), tasks.index(inspection))
        self.assertLess(tasks.index(inspection), tasks.index(guard))
        self.assertLess(tasks.index(guard), tasks.index(disable))
        self.assertEqual(guard["loop"], "{{ scbench_unit_enablement.results }}")
        self.assertEqual(disable["loop"], guard["loop"])
        self.assertEqual(
            disable["ansible.builtin.systemd_service"],
            {
                "name": "simplestchat-{{ item.item }}.service",
                "enabled": False,
            },
        )
        self.assertNotIn(
            "changed_when", disable, "An actual disable must keep normal change reporting"
        )

        environment = Environment(undefined=StrictUndefined, autoescape=False)  # noqa: S701 - configuration, not HTML.
        accepted = [
            environment.compile_expression(value)
            for value in strings(guard, "ansible.builtin.assert", "that")
        ]
        should_disable = environment.compile_expression(string(disable, "when"))
        inspection_failed = environment.compile_expression(string(inspection, "failed_when"))
        for state, rc, needs_change in [
            ("enabled", 0, True),
            ("enabled-runtime", 0, True),
            ("disabled", 1, False),
            ("static", 0, False),
        ]:
            with self.subTest(state=state):
                result: JsonObject = {"stdout": f"{state}\n", "rc": rc, "item": "benchmark"}
                self.assertFalse(evaluate(inspection_failed, scbench_unit_enablement=result))
                self.assertTrue(all(evaluate(check, item=result) for check in accepted))
                self.assertEqual(evaluate(should_disable, item=result), needs_change)
        for state in [
            "masked",
            "masked-runtime",
            "linked",
            "linked-runtime",
            "indirect",
            "generated",
            "transient",
            "not-found",
            "",
        ]:
            with self.subTest(unexpected_state=state):
                result = {"stdout": state, "rc": 1, "item": "benchmark"}
                self.assertFalse(all(evaluate(check, item=result) for check in accepted))
                self.assertFalse(evaluate(should_disable, item=result))
        self.assertTrue(inspection_failed(scbench_unit_enablement={"rc": 4}))


if __name__ == "__main__":
    os.environ["ANSIBLE_CONFIG"] = str(ROOT / "ansible.cfg")
    _ = unittest.main()
