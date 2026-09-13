"""Offline policy checks for disposable-host release integration in CI."""

from pathlib import Path
import re
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[3]
WORKFLOW = ROOT / ".github/workflows/ci.yml"
PREPARE = "Install isolated release-container checks"
INTEGRATION = "App-only release and rollback on a disposable host"
RETENTION = "Preserve sanitized release-container report"
RUN_COMMAND = (
    'sudo "${RUNNER_TEMP}/release-container-controller/bin/python" -B \\\n'
    '  build/test-release-container.py --disposable-host \\\n'
    '  --image "${PRODUCTION_IMAGE}" --output "${RUNNER_TEMP}/release-container"\n'
)


class ReleaseContainerCiTests(unittest.TestCase):
    def setUp(self):
        # BaseLoader preserves GitHub's YAML 1.2 `on` key and compares scalars
        # without PyYAML's YAML 1.1 implicit boolean conversions.
        self.workflow = yaml.load(WORKFLOW.read_text(), Loader=yaml.BaseLoader)
        self.job = self.workflow["jobs"]["deployment"]
        self.steps = self.job["steps"]

    def step(self, name):
        matches = [step for step in self.steps if step.get("name") == name]
        self.assertEqual(len(matches), 1, f"Expected exactly one {name!r} step")
        return matches[0]

    def test_fresh_host_and_job_deadline_include_the_existing_build(self):
        self.assertEqual(self.job["runs-on"], "ubuntu-24.04")
        self.assertEqual(self.job["timeout-minutes"], "75")
        self.assertNotIn("container", self.job, "The fixture uses the runner's own Docker daemon")
        self.assertNotIn("services", self.job)
        self.assertEqual(self.job["env"]["PRODUCTION_IMAGE"], "simplestchat-ci:production")
        self.assertNotIn("continue-on-error", self.job)

    def test_existing_image_is_built_and_smoked_once_before_release_integration(self):
        expected = [
            "Build production container", "Verify production image contents and user",
            "Production startup, migration and database-backed API smoke",
            PREPARE, INTEGRATION, RETENTION,
        ]
        indexes = [self.steps.index(self.step(name)) for name in expected]
        self.assertEqual(indexes, sorted(indexes))
        build = self.step("Build production container")["run"]
        self.assertIn('--tag "${PRODUCTION_IMAGE}" .', build)
        build_steps = [step for step in self.steps
                       if re.search(r"\bdocker\s+(?:build|buildx\s+build)\b", step.get("run", ""))]
        self.assertEqual(build_steps, [self.step("Build production container")])

    def test_dependencies_use_existing_pinned_controller_requirements_in_an_isolated_venv(self):
        prepare = self.step(PREPARE)
        self.assertEqual(prepare["timeout-minutes"], "5")
        self.assertEqual(prepare["run"], (
            "sudo apt-get update\n"
            "sudo apt-get install -y python3-venv openssl curl ca-certificates util-linux\n"
            'python3 -m venv "${RUNNER_TEMP}/release-container-controller"\n'
            '"${RUNNER_TEMP}/release-container-controller/bin/pip" install -r ops/ansible/requirements.txt\n'
        ))
        self.assertNotIn("continue-on-error", prepare)
        self.assertNotIn("if", prepare)

    def test_root_disposable_opt_in_is_explicit_bounded_and_preserves_failure(self):
        integration = self.step(INTEGRATION)
        self.assertEqual(integration["run"], RUN_COMMAND)
        self.assertEqual(integration["timeout-minutes"], "10")
        self.assertNotIn("continue-on-error", integration)
        self.assertNotIn("if", integration, "Do not silently skip a required release check")
        self.assertNotIn("env", integration, "No deployment secrets or helper overrides enter the fixture")

    def test_only_sanitized_report_is_retained_even_on_failure(self):
        retention = self.step(RETENTION)
        self.assertRegex(retention["uses"], r"^actions/upload-artifact@[a-f0-9]{40}$")
        self.assertEqual(retention["if"], "${{ !cancelled() }}")
        self.assertEqual(retention["with"], {
            "name": "release-container",
            "path": "${{ runner.temp }}/release-container/report.json",
            "if-no-files-found": "warn", "retention-days": "7",
        })
        self.assertNotIn("run", retention, "Retention must not rewrite a failed test outcome")
        self.assertNotIn("continue-on-error", retention)
        uploads = [step for step in self.steps if "upload-artifact@" in step.get("uses", "")]
        self.assertEqual({step["with"]["path"] for step in uploads}, {
            "${{ runner.temp }}/container-smoke",
            "${{ runner.temp }}/release-container/report.json",
        }, "Do not upload private release fixture directories or broad runner paths")

    def test_read_only_permissions_pinned_actions_and_no_remote_deployment(self):
        self.assertEqual(self.workflow["permissions"], {"contents": "read"})
        self.assertNotIn("permissions", self.job)
        self.assertNotIn("environment", self.job)
        for step in self.steps:
            if "uses" in step:
                self.assertRegex(step["uses"], r"^actions/(?:checkout|upload-artifact)@[a-f0-9]{40}$")
            self.assertNotIn("permissions", step)
            self.assertNotIn("environment", step)
        checkout = next(step for step in self.steps if step.get("uses", "").startswith("actions/checkout@"))
        self.assertEqual(checkout["with"], {"persist-credentials": "false"})
        commands = "\n".join(step.get("run", "") for step in self.steps)
        self.assertNotRegex(commands, r"\b(?:ssh|scp|ansible-playbook|kubectl|reboot)\b")
        self.assertNotRegex(commands, r"\bdocker\s+(?:push|login)\b")
        self.assertNotRegex(str(self.job), r"\$\{\{\s*secrets\.")


if __name__ == "__main__":
    unittest.main()
