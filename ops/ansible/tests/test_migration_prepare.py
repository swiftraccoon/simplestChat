"""Offline migration preparation ordering, target and secret-handling contracts."""

import re
import unittest
from functools import cached_property
from pathlib import Path
from typing import TYPE_CHECKING, cast

from jinja2 import Environment, StrictUndefined, UndefinedError
from test_support import obj, objects, strings, yaml_value

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
