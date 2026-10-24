"""Offline release-workflow policy checks; never invoke GitHub or Docker."""

import re
import unittest
from functools import cached_property
from pathlib import Path

from test_support import obj, objects, string, yaml_value

# isort: split
from release_json import JsonObject

ROOT = Path(__file__).resolve().parents[3]
WORKFLOW = ROOT / ".github/workflows/release-artifact.yml"
TOOLCHAIN_ACTION = "dtolnay/rust-toolchain@89b12181fb390509a0842a86cc55eeb8eb928c1d"
PREPARATION_COMMAND = (
    "cargo build --locked --release -p simplestchat-ops --bin simplestchat-ops \\\n"
    + '  --target-dir "$RUNNER_TEMP/release-builder-build"\n'
)
BUILDER_COMMAND = (
    '"$RUNNER_TEMP/release-builder-build/release/simplestchat-ops" release build \\\n'
    + '  --output "${RUNNER_TEMP}/simplestchat-release"\n'
)


class ReleaseWorkflowTests(unittest.TestCase):
    """Verify the release workflow contract offline."""

    @cached_property
    def workflow(self) -> JsonObject:
        """Read this test's workflow fixture with GitHub scalar semantics."""
        # GitHub uses YAML 1.2: BaseLoader keeps its `on` key intact instead of
        # interpreting it as the YAML 1.1 boolean used by PyYAML's SafeLoader.
        return obj(yaml_value(WORKFLOW.read_text(), scalars_as_strings=True))

    @cached_property
    def job(self) -> JsonObject:
        """Select the artifact build job with checked object boundaries."""
        return obj(self.workflow, "jobs", "artifact")

    @cached_property
    def steps(self) -> list[JsonObject]:
        """Require every workflow step to be a mapping."""
        return objects(self.job, "steps")

    def test_release_build_is_manual_only_with_read_only_repository_permissions(self) -> None:
        """Verify release build is manual only with read only repository permissions."""
        self.assertEqual(set(obj(self.workflow, "on")), {"workflow_dispatch"})
        self.assertEqual(self.workflow["permissions"], {"contents": "read"})
        self.assertEqual(set(obj(self.workflow, "jobs")), {"artifact"})
        self.assertNotIn("permissions", self.job, "The job must not escalate the workflow token")
        self.assertEqual(self.job["runs-on"], "ubuntu-24.04")
        self.assertEqual(self.job["timeout-minutes"], "75")

    def test_only_checkout_toolchain_and_retention_actions_are_used_and_pinned(self) -> None:
        """Pin the selected compiler bootstrap alongside checkout and retention."""
        actions = [step for step in self.steps if "uses" in step]
        self.assertEqual(len(actions), 3)
        self.assertEqual(
            {string(step, "uses").split("@")[0] for step in actions},
            {
                "actions/checkout",
                "actions/upload-artifact",
                "dtolnay/rust-toolchain",
            },
        )
        for step in actions:
            self.assertRegex(
                string(step, "uses"),
                r"^(?:actions/(?:checkout|upload-artifact)|dtolnay/rust-toolchain)@[a-f0-9]{40}$",
            )
        checkout = next(
            step for step in actions if string(step, "uses").startswith("actions/checkout@")
        )
        self.assertEqual(checkout["with"], {"persist-credentials": "false"})
        toolchain = next(step for step in actions if string(step, "uses") == TOOLCHAIN_ACTION)
        self.assertEqual(toolchain["with"], {"toolchain": "1.99.0"})
        self.assertEqual(toolchain["timeout-minutes"], "2")

    def test_native_preparation_precedes_the_exact_production_builder(self) -> None:
        """Keep cold source compilation outside the operation's original entry clock."""
        commands = [step for step in self.steps if "run" in step]
        self.assertEqual(len(commands), 2)
        preparation, builder = commands
        self.assertEqual(preparation["run"], PREPARATION_COMMAND)
        self.assertEqual(preparation["timeout-minutes"], "6")
        self.assertEqual(builder["run"], BUILDER_COMMAND)
        self.assertEqual(builder["timeout-minutes"], "65")
        self.assertNotIn(
            "if", builder, "An unconditional selected-commit build must not be silently skipped"
        )
        for step in commands:
            self.assertNotIn("if", step)
            self.assertNotIn("continue-on-error", step)
        self.assertNotIn("continue-on-error", self.job)
        self.assertEqual(len(self.steps), 5)
        self.assertLess(self.steps.index(self.steps[0]), self.steps.index(builder))
        self.assertLess(self.steps.index(preparation), self.steps.index(builder))
        toolchain = next(step for step in self.steps if step.get("uses") == TOOLCHAIN_ACTION)
        self.assertLess(self.steps.index(toolchain), self.steps.index(preparation))
        self.assertTrue(string(self.steps[0], "uses").startswith("actions/checkout@"))

    def test_ci_export_uses_the_tested_image_with_separate_native_preparation(self) -> None:
        """Keep the existing six-minute export bound and immutable image selection."""
        workflow = obj(
            yaml_value((ROOT / ".github/workflows/ci.yml").read_text(), scalars_as_strings=True)
        )
        steps = objects(obj(workflow, "jobs", "deployment"), "steps")
        preparation = next(step for step in steps if step.get("run") == PREPARATION_COMMAND)
        self.assertEqual(preparation["timeout-minutes"], "6")
        export = next(
            step
            for step in steps
            if step.get("name") == "Export the tested immutable production image"
        )
        self.assertEqual(export["timeout-minutes"], "6")
        self.assertEqual(
            export["run"],
            '"$RUNNER_TEMP/release-builder-build/release/simplestchat-ops" '
            + 'release build \\\n  --image-id "${PRODUCTION_IMAGE}" \\\n'
            + '  --timeout-seconds 300 --output "${RUNNER_TEMP}/simplestchat-release"\n',
        )
        smoke = next(
            step
            for step in steps
            if step.get("name") == "Production startup, migration and database-backed API smoke"
        )
        self.assertLess(steps.index(preparation), steps.index(smoke))
        self.assertLess(steps.index(smoke), steps.index(export))

    def test_original_failed_outcome_is_retained_without_turning_build_failure_into_success(
        self,
    ) -> None:
        """Verify original failed outcome is retained without turning build failure into success."""
        retention = next(
            step
            for step in self.steps
            if string(step.get("uses", "")).startswith("actions/upload-artifact@")
        )
        self.assertEqual(
            retention["if"],
            "${{ !cancelled() }}",
            "Retain failed-build evidence without retrying cancelled jobs",
        )
        self.assertEqual(
            retention["with"],
            {
                "name": "simplestchat-development-${{ github.sha }}",
                "path": "${{ runner.temp }}/simplestchat-release",
                "if-no-files-found": "error",
                "compression-level": "0",
                "retention-days": "14",
            },
        )
        self.assertIs(retention, self.steps[-1])
        self.assertNotIn("continue-on-error", retention)
        self.assertNotIn(
            "run", retention, "Retention must not rewrite the builder's original outcome"
        )

    def test_no_deployment_publication_credentials_or_service_side_effects_are_configured(
        self,
    ) -> None:
        """Verify no deployment publication credentials or service side effects are configured."""
        for scope in (self.workflow, self.job, *self.steps):
            self.assertNotIn("environment", scope)
            self.assertNotIn("env", scope)
            self.assertNotIn("services", scope)
            self.assertNotIn("secrets", scope)
        self.assertNotRegex(WORKFLOW.read_text(), r"\$\{\{\s*secrets\.")
        commands = "\n".join(string(step.get("run", "")) for step in self.steps)
        self.assertIsNone(
            re.search(
                r"\b(?:ssh|scp|ansible-playbook|push|publish|deploy|login|reboot)\b", commands
            )
        )
        # The sole publication is GitHub's retained build artifact, not a
        # registry push, release creation, VPS command or deploy environment.
        self.assertEqual(commands, "\n\n" + PREPARATION_COMMAND + "\n" + BUILDER_COMMAND + "\n")


if __name__ == "__main__":
    _ = unittest.main()
