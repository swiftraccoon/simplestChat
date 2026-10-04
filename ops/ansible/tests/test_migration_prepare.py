"""Offline migration preparation ordering, target and secret-handling contracts."""

import re
import unittest
from functools import cached_property
from pathlib import Path
from typing import TYPE_CHECKING, cast

from jinja2 import Environment, StrictUndefined, UndefinedError
from test_support import array, obj, objects, string, strings, yaml_value

# isort: split
import release_preflight
from release_json import JsonObject

if TYPE_CHECKING:
    from collections.abc import MutableMapping

ROOT = Path(__file__).resolve().parents[1]


class MigrationPrepareTests(unittest.TestCase):
    """Check the actual playbook's safety boundaries without any host operations."""

    @cached_property
    def play(self) -> JsonObject:
        """Read the maintained playbook once per test."""
        return obj(yaml_value((ROOT / "migration-prepare.yml").read_text()), 0)

    def block(self, action: str) -> list[JsonObject]:
        """Find the task block selected by an actual migration action."""
        return next(
            objects(task, "block")
            for task in objects(self.play, "tasks")
            if f"scmig_action == '{action}'" in str(task["when"])
        )

    def test_selection_guard_refuses_check_mode_extra_hosts_and_bad_operation(self) -> None:
        """Evaluate real guard expressions against ambiguous and malformed target selections."""
        guard = objects(self.play, "pre_tasks")[0]
        environment = Environment(undefined=StrictUndefined, autoescape=True)

        def matches(value: str, pattern: str) -> bool:
            return re.match(pattern, value) is not None

        cast("MutableMapping[str, object]", environment.tests)["match"] = matches
        cast("MutableMapping[str, object]", environment.filters)["bool"] = bool
        checks = [
            environment.compile_expression(value)
            for value in strings(guard, "ansible.builtin.assert", "that")
        ]
        baseline: JsonObject = {
            "ansible_check_mode": False,
            "ansible_play_hosts_all": ["migration_source", "migration_target"],
            "inventory_hostname": "migration_source",
            "scmig_action": "inspect",
            "scmig_run_id": "a" * 32,
            "scmig_revision": "b" * 40,
            "scmig_source_origin": "https://chat.example.test",
            "scmig_destination_origin": "https://next.chat.example.test",
            "scmig_evidence": "/private/controller/evidence",
            "scpub_enabled": True,
            "scpub_config": "/etc/simplestchat-public",
            "scpub_root": "/srv/simplestchat-public",
        }
        self.assertTrue(all(check(**baseline) for check in checks))
        self.assertTrue(
            all(
                check(**(baseline | {"scmig_destination_origin": baseline["scmig_source_origin"]}))
                for check in checks
            )
        )
        for change in (
            {"ansible_check_mode": True},
            {"ansible_play_hosts_all": ["migration_source", "migration_target", "other"]},
            {"ansible_play_hosts_all": ["migration_target"]},
            {"inventory_hostname": "other"},
            {"scmig_action": "deploy"},
            {"scmig_run_id": "../elsewhere"},
            {"scmig_revision": "main"},
        ):
            with self.subTest(change=change):
                try:
                    passed = all(check(**(baseline | change)) for check in checks)
                except (UndefinedError, TypeError):
                    passed = False
                self.assertFalse(passed)

    def test_inspection_is_read_only_remote_and_receipt_is_private_local(self) -> None:
        """The source remains live and physical identities are compared before preparation."""
        tasks = self.block("inspect")
        command = next(task for task in tasks if "ansible.builtin.command" in task)
        self.assertFalse(command["changed_when"])
        self.assertIn("migration_inspect.py", str(command))
        self.assertFalse(
            any("ansible.builtin.file" in task or "ansible.builtin.fetch" in task for task in tasks)
        )
        receipt = next(task for task in tasks if "ansible.builtin.copy" in task)
        self.assertEqual(receipt["delegate_to"], "localhost")
        self.assertFalse(receipt["become"])
        self.assertEqual(string(receipt, "ansible.builtin.copy", "mode"), "0600")
        guard = next(task for task in tasks if "ansible.builtin.assert" in task)
        self.assertIn("machineId !=", " ".join(strings(guard, "ansible.builtin.assert", "that")))
        self.assertEqual(self.play["strategy"], "linear")
        self.assertTrue(self.play["any_errors_fatal"])

    def test_bootstrap_preserves_unfinished_work_and_complete_helper_contract(self) -> None:
        """The refusal guard precedes file writes and the canonical helper set is complete."""
        tasks = self.block("bootstrap")
        guard = tasks[0]
        self.assertIn("ansible.builtin.command", guard)
        self.assertFalse(guard["changed_when"])
        source = "\n".join(strings(guard, "ansible.builtin.command", "argv"))
        for token in (
            "LOCK_EX | fcntl.LOCK_NB",
            "record.get('finalized') is True",
            "request.exists()",
            "release-state.json",
            "current.json",
        ):
            self.assertIn(token, source)
        install = next(
            task
            for task in tasks
            if obj(task.get("ansible.builtin.copy", {})).get("src") == "{{ item }}"
        )
        required = release_preflight.BASE_HELPERS | release_preflight.FETCH_HELPERS
        self.assertLessEqual(
            required | {"migration_snapshot.py", "migrate_public.py", "backup_public.py"},
            set(strings(install, "loop")),
        )

    def test_secret_copy_requires_exact_digest_without_owner_or_secret_logging(self) -> None:
        """Credentials stay private and an existing differing destination cannot be overwritten."""
        tasks = self.block("bootstrap")
        fetch = next(task for task in tasks if "ansible.builtin.fetch" in task)
        self.assertTrue(fetch["no_log"])
        copies = [
            task
            for task in tasks
            if obj(task.get("ansible.builtin.copy", {})).get("dest")
            == "/etc/simplestchat-public/secrets.json"
        ]
        self.assertEqual(len(copies), 1)
        target = copies[0]
        self.assertEqual(target["when"], "inventory_hostname == 'migration_target'")
        self.assertTrue(target["no_log"])
        self.assertFalse(obj(target, "ansible.builtin.copy")["force"])
        self.assertEqual(string(target, "ansible.builtin.copy", "mode"), "0600")
        self.assertNotIn("owner.json", str(tasks))
        guards = [
            task for task in tasks if "ansible.builtin.assert" in task and "checksum" in str(task)
        ]
        self.assertEqual(len(guards), 3)
        self.assertTrue(all(task["no_log"] for task in guards))
        self.assertLess(tasks.index(guards[1]), tasks.index(target))
        self.assertGreater(tasks.index(guards[2]), tasks.index(target))

    def test_same_origin_tls_transfer_is_private_and_precedes_staging(self) -> None:
        """The exact hostname subtree is validated on both hosts without logging its payload."""
        tasks = objects(self.play, "tasks")
        tls = next(
            task
            for task in tasks
            if task["name"] == "Preserve the unchanged hostname certificate before DNS cutover"
        )
        self.assertEqual(
            tls["when"],
            ["scmig_action == 'bootstrap'", "scmig_source_origin == scmig_destination_origin"],
        )
        steps = objects(tls, "block")
        self.assertTrue(all(step["no_log"] for step in steps))
        self.assertEqual(steps[0]["when"], "inventory_hostname == 'migration_source'")
        self.assertEqual(steps[-1]["when"], "inventory_hostname == 'migration_target'")
        self.assertIn("export", strings(steps[0], "ansible.builtin.command", "argv"))
        self.assertIn("import", strings(steps[-1], "ansible.builtin.command", "argv"))
        copy = next(step for step in steps if "ansible.builtin.copy" in step)
        self.assertFalse(obj(copy, "ansible.builtin.copy")["force"])
        self.assertEqual(string(copy, "ansible.builtin.copy", "mode"), "0600")
        stage = next(
            task
            for task in tasks
            if task["name"]
            == "Transfer and stage the exact verified CI artifact only on the destination"
        )
        self.assertLess(tasks.index(tls), tasks.index(stage))
        self.assertIn("not (scmig_force_release | default(false) | bool)", strings(stage, "when"))

    def test_stage_uses_verified_receiver_then_only_bounded_image_import(self) -> None:
        """Fresh hosts use the signed receiver and real stage action without builds or startup."""
        tasks = self.block("stage")
        commands = [task for task in tasks if "ansible.builtin.command" in task]
        self.assertEqual(len(commands), 2)
        transfer, stage = commands
        fetch_args = strings(transfer, "ansible.builtin.command", "argv")
        self.assertIn("{{ playbook_dir }}/../../build/fetch-release.py", fetch_args)
        self.assertEqual(
            fetch_args[-2:], ["--verified-directory", "{{ scpub_release_verified_directory }}"]
        )
        self.assertEqual(transfer["delegate_to"], "localhost")
        self.assertFalse(transfer["become"])
        stage_args = array(stage, "ansible.builtin.command", "argv")
        self.assertIn("--property=RuntimeMaxSec=600", stage_args)
        self.assertEqual(
            stage_args[-3:],
            [
                "/usr/local/libexec/simplestchat-public/release-public.py",
                "stage",
                "{{ scmig_revision }}",
            ],
        )
        self.assertNotIn("deploy", stage_args)
