"""Keep the fixed disposable database-check workflow boundary explicit."""

from __future__ import annotations

import unittest

from test_support import ROOT, obj, objects, string, yaml_value


class DatabaseCheckWorkflowTests(unittest.TestCase):
    """Guards, ordering and container opt-ins survive the typed CLI move."""

    def test_three_checks_keep_the_original_cache_guards_and_restore_opt_in(self) -> None:
        """Select only the three maintained suites after pinned compiler setup."""
        workflow = yaml_value(
            (ROOT / ".github/workflows/ci.yml").read_text(), scalars_as_strings=True
        )
        job = obj(workflow, "jobs", "automation")
        self.assertEqual(job["timeout-minutes"], "10")
        steps = objects(job, "steps")
        compiler = next(
            index
            for index, step in enumerate(steps)
            if step.get("name") == "Install the pinned Rust toolchain for CI receipts"
        )
        names = {
            "Verify operational alert history in disposable PostgreSQL": "monitoring",
            "Verify isolated backup restoration with pinned PostgreSQL": "restore",
            "Verify server migration data preservation in disposable PostgreSQL": "migration",
        }
        selected: list[int] = []
        for name, check in names.items():
            matches = [step for step in steps if step.get("name") == name]
            self.assertEqual(len(matches), 1)
            step = matches[0]
            self.assertGreater(steps.index(step), compiler)
            self.assertEqual(step["if"], "steps.verified.outputs.cache-hit != 'true'")
            self.assertEqual(
                string(step, "run"),
                f"cargo run --locked -q -p xtask -- ci database-check {check}",
            )
            self.assertNotIn("continue-on-error", step)
            self.assertNotIn("timeout-minutes", step)
            self.assertEqual(
                obj(step.get("env", {})),
                {"RESTORE_CONTAINER_E2E": "1"} if check == "restore" else {},
            )
            selected.append(steps.index(step))
        self.assertEqual(selected, sorted(selected))
        commands = [step for step in steps if "ci database-check" in string(step.get("run", ""))]
        self.assertEqual(len(commands), 3)
