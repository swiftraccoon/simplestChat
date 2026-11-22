"""Offline migration preparation ordering, target and secret-handling contracts."""

import re
import unittest
from functools import cached_property
from pathlib import Path
from typing import TYPE_CHECKING, cast

from jinja2 import Environment, StrictUndefined, UndefinedError
from test_support import obj, objects, strings, yaml_value

# isort: split
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
