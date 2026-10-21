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
            if "cargo run --locked -q -p xtask -- check --python" in string(step.get("run", ""))
        )
        self.assertLess(steps.index(install), steps.index(gate))
        rust = next(
            step
            for step in steps
            if step.get("name") == "Install the pinned Rust toolchain for the test harness"
        )
        self.assertLess(steps.index(rust), steps.index(gate))
        self.assertEqual(rust["run"], "rustup toolchain install --profile minimal")
        self.assertEqual(rust["if"], gate["if"])
        command = string(gate, "run")
        self.assertLess(
            command.index("cargo run --locked -q -p xtask -- check --python"),
            command.index("python -m unittest discover"),
        )
        self.assertEqual(
            string(gate, "env", "PYTHON_CHECK_ENV"), "${{ runner.temp }}/ansible-controller"
        )
        for scope in (job, gate):
            self.assertNotIn("continue-on-error", scope)
        self.assertNotIn("if", job)
        self.assertEqual(gate["if"], "steps.verified.outputs.cache-hit != 'true'")
        verified = next(
            step for step in steps if "ci_verified.py verify" in string(step.get("run", ""))
        )
        self.assertEqual(verified["if"], "steps.verified.outputs.cache-hit == 'true'")
        self.assertEqual(string(verified, "env", "CI_CACHE_SCOPE"), "automation")
        self.assertLess(steps.index(verified), steps.index(gate))
        self.assertNotIn("continue-on-error", verified)


if __name__ == "__main__":
    _ = unittest.main()
