"""Offline release-workflow policy checks; never invoke GitHub or Docker."""

from pathlib import Path
import re
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[3]
WORKFLOW = ROOT / ".github/workflows/release-artifact.yml"
BUILDER_COMMAND = 'python3 build/build-release.py --output "${RUNNER_TEMP}/simplestchat-release"'


class ReleaseWorkflowTests(unittest.TestCase):
    def setUp(self):
        # GitHub uses YAML 1.2: BaseLoader keeps its `on` key intact instead of
        # interpreting it as the YAML 1.1 boolean used by PyYAML's SafeLoader.
        self.workflow = yaml.load(WORKFLOW.read_text(), Loader=yaml.BaseLoader)
        self.job = self.workflow["jobs"]["artifact"]
        self.steps = self.job["steps"]

    def test_release_build_is_manual_only_with_read_only_repository_permissions(self):
        self.assertEqual(set(self.workflow["on"]), {"workflow_dispatch"})
        self.assertEqual(self.workflow["permissions"], {"contents": "read"})
        self.assertEqual(set(self.workflow["jobs"]), {"artifact"})
        self.assertNotIn("permissions", self.job, "The job must not escalate the workflow token")
        self.assertEqual(self.job["runs-on"], "ubuntu-24.04")
        self.assertEqual(self.job["timeout-minutes"], "75")

    def test_only_checkout_and_artifact_retention_actions_are_used_and_pinned(self):
        actions = [step for step in self.steps if "uses" in step]
        self.assertEqual(len(actions), 2)
        self.assertEqual({step["uses"].split("@")[0] for step in actions}, {
            "actions/checkout", "actions/upload-artifact",
        })
        for step in actions:
            self.assertRegex(step["uses"], r"^actions/(?:checkout|upload-artifact)@[a-f0-9]{40}$")
        checkout = next(step for step in actions if step["uses"].startswith("actions/checkout@"))
        self.assertEqual(checkout["with"], {"persist-credentials": "false"})

    def test_exact_production_builder_is_the_only_shell_step(self):
        commands = [step for step in self.steps if "run" in step]
        self.assertEqual(len(commands), 1)
        builder = commands[0]
        self.assertEqual(builder["run"], BUILDER_COMMAND)
        self.assertNotIn("if", builder, "An unconditional selected-commit build must not be silently skipped")
        self.assertNotIn("continue-on-error", builder)
        self.assertNotIn("continue-on-error", self.job)
        self.assertEqual(len(self.steps), 3)
        self.assertLess(self.steps.index(self.steps[0]), self.steps.index(builder))
        self.assertTrue(self.steps[0]["uses"].startswith("actions/checkout@"))

    def test_original_failed_outcome_is_retained_without_turning_build_failure_into_success(self):
        retention = next(step for step in self.steps if step.get("uses", "").startswith("actions/upload-artifact@"))
        self.assertEqual(retention["if"], "${{ !cancelled() }}",
                         "Explicit status condition retains failed-build evidence without retrying cancelled jobs")
        self.assertEqual(retention["with"], {
            "name": "simplestchat-production-${{ github.sha }}",
            "path": "${{ runner.temp }}/simplestchat-release",
            "if-no-files-found": "error", "compression-level": "0", "retention-days": "14",
        })
        self.assertIs(retention, self.steps[-1])
        self.assertNotIn("continue-on-error", retention)
        self.assertNotIn("run", retention, "Retention must not rewrite the builder's original outcome")

    def test_no_deployment_publication_credentials_or_service_side_effects_are_configured(self):
        for scope in (self.workflow, self.job, *self.steps):
            self.assertNotIn("environment", scope)
            self.assertNotIn("env", scope)
            self.assertNotIn("services", scope)
            self.assertNotIn("secrets", scope)
        self.assertNotRegex(WORKFLOW.read_text(), r"\$\{\{\s*secrets\.")
        commands = "\n".join(step.get("run", "") for step in self.steps)
        self.assertIsNone(re.search(r"\b(?:ssh|scp|ansible-playbook|push|publish|deploy|login|reboot)\b", commands))
        # The sole publication is GitHub's retained build artifact, not a
        # registry push, release creation, VPS command or deploy environment.
        self.assertEqual(commands, "\n" + BUILDER_COMMAND + "\n")


if __name__ == "__main__":
    unittest.main()
