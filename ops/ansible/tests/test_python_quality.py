"""Keep Python checker policy and CI wiring aligned with the Rust quality gate.

The actual gate discovery, tool selection and failure sequencing are exercised
by xtask's owned-checkout tests in xtask/src/check/check_tests.rs.
"""

from __future__ import annotations

import tomllib
import unittest
from typing import cast

from test_support import ROOT, obj, objects, string, strings, yaml_value

# Bootstrap flat checkout imports before loading JSON validators.
# isort: split
from release_json import json_value


class PythonPolicyTests(unittest.TestCase):
    """Keep shared strict settings, tool pins and CI gate wiring aligned."""

    def test_branch_subject_checks_the_head_before_any_cached_gate(self) -> None:
        """Keep the typed byte-policy check unconditional after the existing bootstrap."""
        workflow = yaml_value(
            (ROOT / ".github/workflows/ci.yml").read_text(), scalars_as_strings=True
        )
        job = obj(workflow, "jobs", "automation")
        steps = objects(job, "steps")
        self.assertEqual(string(job, "timeout-minutes"), "10")
        self.assertEqual(string(steps[0], "with", "persist-credentials"), "false")
        self.assertEqual(string(steps[0], "with", "fetch-depth"), "2")
        self.assertEqual(string(steps[1], "if"), "${{ env.ACT }}")
        self.assertEqual(string(steps[1], "run"), "build/ci-local-docker.sh")
        rust, subject, fingerprint = steps[2:5]
        self.assertEqual(string(rust, "name"), "Install the pinned Rust toolchain for CI receipts")
        self.assertEqual(
            string(rust, "uses"),
            "dtolnay/rust-toolchain@89b12181fb390509a0842a86cc55eeb8eb928c1d",
        )
        self.assertEqual(obj(rust, "with"), {"toolchain": "1.99.0"})
        self.assertEqual(string(subject, "name"), "Check the branch tip commit subject")
        self.assertEqual(
            obj(subject, "env"),
            {"COMMIT_REVISION": "${{ github.event.pull_request.head.sha || github.sha }}"},
        )
        self.assertEqual(
            string(subject, "run"), "cargo run --locked -q -p xtask -- ci commit-subject"
        )
        self.assertEqual(string(fingerprint, "id"), "verified-inputs")
        for scope in (job, rust, subject):
            self.assertNotIn("if", scope)
            self.assertNotIn("continue-on-error", scope)
        self.assertEqual(
            sum(step.get("name") == "Check the branch tip commit subject" for step in steps), 1
        )

    def test_configuration_has_full_rules_and_no_machine_specific_environment(self) -> None:
        """Require strict rule sets without blanket type suppressions or local venv bindings."""
        config = json_value(cast("object", tomllib.loads((ROOT / "pyproject.toml").read_text())))
        ruff = obj(config, "tool", "ruff")
        self.assertEqual(strings(ruff, "lint", "select"), ["ALL"])
        self.assertNotIn("build", strings(ruff, "exclude"))
        self.assertNotIn("build/", strings(ruff, "exclude"))
        based = obj(config, "tool", "basedpyright")
        self.assertEqual(based["typeCheckingMode"], "all")
        self.assertNotIn("venvPath", based)
        self.assertNotIn("venv", based)
        self.assertNotIn("ignore", based)
        self.assertEqual(based["stubPath"], "typings")
        self.assertTrue(
            {
                "build",
                "ops/ansible/files",
                "ops/ansible/callback_plugins",
                "ops/ansible/tests",
                "typings",
            }.issubset(strings(based, "include"))
        )
        for name, value in based.items():
            if name.startswith("report") and name != "reportImplicitRelativeImport":
                self.assertNotIn(value, (False, "none"), name)

    def test_checker_requirements_pin_versions_and_share_controller_dependencies(self) -> None:
        """Install one pinned checker/controller environment without VPS tool additions."""
        lines = {
            line.strip().removesuffix("\\").strip()
            for line in (ROOT / "build/python-requirements.txt").read_text().splitlines()
            if line.strip() and not line.startswith("#")
        }
        self.assertIn("-r ../ops/ansible/requirements.txt", lines)
        self.assertIn("--require-hashes", lines)
        self.assertIn("--only-binary=:all:", lines)
        for name in ("ruff", "basedpyright", "types-pyyaml"):
            self.assertEqual(sum(line.startswith(name + "==") for line in lines), 1)
        config = json_value(cast("object", tomllib.loads((ROOT / "pyproject.toml").read_text())))
        self.assertIn("ruff" + string(config, "tool", "ruff", "required-version"), lines)

    def test_ci_installs_and_runs_the_same_gate_before_offline_tests(self) -> None:
        """Use the temporary CI environment explicitly without permitting ignored failures."""
        workflow = yaml_value(
            (ROOT / ".github/workflows/ci.yml").read_text(), scalars_as_strings=True
        )
        job = obj(workflow, "jobs", "automation")
        steps = objects(job, "steps")
        install = next(
            step
            for step in steps
            if "install -r build/python-requirements.txt" in string(step.get("run", ""))
        )
        gate = next(
            step
            for step in steps
            if "cargo run --locked -q -p xtask -- ci automation" in string(step.get("run", ""))
        )
        self.assertLess(steps.index(install), steps.index(gate))
        rust = next(
            step
            for step in steps
            if step.get("name") == "Install the pinned Rust toolchain for CI receipts"
        )
        self.assertLess(steps.index(rust), steps.index(gate))
        self.assertLess(steps.index(rust), steps.index(install))
        self.assertEqual(install["if"], "steps.verified.outputs.cache-hit != 'true'")
        self.assertEqual(
            string(install, "run").splitlines(),
            [
                "sudo apt-get update",
                "sudo apt-get install -y python3-venv shellcheck jq postgresql",
                'python3 -m venv "${RUNNER_TEMP}/ansible-controller"',
                '"${RUNNER_TEMP}/ansible-controller/bin/pip" install -r '
                + "build/python-requirements.txt",
                "cargo run --locked -q -p xtask -- ci controller-path",
            ],
        )
        self.assertNotIn("continue-on-error", install)
        self.assertEqual(
            rust["uses"],
            "dtolnay/rust-toolchain@89b12181fb390509a0842a86cc55eeb8eb928c1d",
        )
        self.assertEqual(string(rust, "with", "toolchain"), "1.99.0")
        self.assertNotIn("if", rust)
        fingerprint = next(step for step in steps if step.get("id") == "verified-inputs")
        self.assertLess(steps.index(rust), steps.index(fingerprint))
        command = string(gate, "run")
        self.assertEqual(command, "cargo run --locked -q -p xtask -- ci automation")
        self.assertEqual(
            string(gate, "env", "PYTHON_CHECK_ENV"), "${{ runner.temp }}/ansible-controller"
        )
        for scope in (job, gate):
            self.assertNotIn("continue-on-error", scope)
        self.assertNotIn("if", job)
        self.assertEqual(gate["if"], "steps.verified.outputs.cache-hit != 'true'")
        verified = next(
            step for step in steps if "xtask -- ci verify" in string(step.get("run", ""))
        )
        self.assertEqual(verified["if"], "steps.verified.outputs.cache-hit == 'true'")
        self.assertEqual(string(verified, "env", "CI_CACHE_SCOPE"), "automation")
        self.assertLess(steps.index(verified), steps.index(gate))
        self.assertNotIn("continue-on-error", verified)


if __name__ == "__main__":
    _ = unittest.main()
