"""Exercise the Python gate with owned Git fixtures and inert checker executables."""

from __future__ import annotations

import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
from dataclasses import dataclass
from pathlib import Path
from typing import cast

from test_support import ROOT, obj, objects, string, strings, yaml_value

# Bootstrap flat checkout imports before loading JSON validators.
# isort: split
from release_json import decode_json, json_value

FAILURE_STATUS = 37
FIXTURE_SOURCES = (
    "build/tracked.py",
    "build/new helper.py",
    "build/nested/untracked.py",
    "ops/ansible/files/host.py",
    "ops/ansible/callback_plugins/observer.py",
    "ops/ansible/tests/test_new\ncase.py",
    "typings/fixture/nested.pyi",
)
FAKE_CHECKER = r"""
import json
import os
import pathlib
import sys

tool, *arguments = sys.argv[1:]
with pathlib.Path(os.environ["FIXTURE_LOG"]).open("a") as output:
    output.write(json.dumps({"tool": tool, "args": arguments, "cwd": os.getcwd()}) + "\n")
if tool == "ruff" and arguments[:2] == ["check", "--"]:
    phase = "lint"
elif tool == "ruff" and arguments[:3] == ["format", "--check", "--"]:
    phase = "format"
elif tool == "basedpyright" and arguments[:1] == ["--pythonpath"] and "--" not in arguments:
    phase = "types"
else:
    print("Unexpected checker arguments", file=sys.stderr)
    sys.exit(91)
if os.environ.get("FIXTURE_FAIL_STAGE") == phase:
    sys.exit(37)
"""


def installed_tool(name: str) -> str:
    """Resolve the local shell or Git needed only for disposable fixture execution."""
    location = shutil.which(name)
    if location is None:
        message = f"Fixture requires installed {name}"
        raise RuntimeError(message)
    return location


def fixture_environment() -> dict[str, str]:
    """Prevent inherited Git selectors from redirecting fixture commands elsewhere."""
    return {
        key: value
        for key, value in os.environ.items()
        if not key.startswith("GIT_") and key not in {"PYTHON_CHECK_ENV", "FIXTURE_FAIL_STAGE"}
    } | {"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull, "GIT_TERMINAL_PROMPT": "0"}


@dataclass(frozen=True)
class CheckerEvent:
    """A recorded inert checker invocation with validated log field types."""

    tool: str
    args: tuple[str, ...]
    cwd: Path


@dataclass(frozen=True)
class GateResult:
    """The real shell gate's status, output and intercepted child invocations."""

    status: int
    stdout: str
    stderr: str
    events: tuple[CheckerEvent, ...]


@dataclass(frozen=True)
class GateFixture:
    """A disposable repository and fake environment, never the actual checkout tools."""

    root: Path
    temporary: Path
    environment: Path
    log: Path
    scratch: Path

    def run(self, *, override: Path | None = None, fail_stage: str = "") -> GateResult:
        """Run the copied gate from another working directory with inert checkers."""
        _ = self.log.write_text("")
        environment = fixture_environment() | {
            "FIXTURE_LOG": str(self.log),
            "FIXTURE_FAIL_STAGE": fail_stage,
            "TMPDIR": str(self.scratch),
        }
        if override is not None:
            environment["PYTHON_CHECK_ENV"] = str(override)
        result = subprocess.run(  # noqa: S603 -- Execute only the copied gate in its disposable Git repository.
            [installed_tool("bash"), str(self.root / "build/check-python.sh")],
            cwd=self.temporary,
            env=environment,
            text=True,
            capture_output=True,
            timeout=10,
            check=False,
        )
        events: list[CheckerEvent] = []
        for line in self.log.read_text().splitlines():
            event = obj(decode_json(line))
            events.append(
                CheckerEvent(
                    string(event, "tool"), tuple(strings(event, "args")), Path(string(event, "cwd"))
                )
            )
        return GateResult(result.returncode, result.stdout, result.stderr, tuple(events))


def gate_fixture(
    test: unittest.TestCase,
    *,
    external_environment: bool = False,
    sources: tuple[str, ...] = FIXTURE_SOURCES,
) -> GateFixture:
    """Copy only the gate into a private Git repository and install fake checker scripts."""
    temporary = tempfile.TemporaryDirectory(prefix="simplestchat-python-gate.")
    test.addCleanup(temporary.cleanup)
    parent = Path(temporary.name).resolve()
    root = parent / "checkout with spaces"
    (root / "build").mkdir(parents=True)
    _ = shutil.copyfile(ROOT / "build/check-python.sh", root / "build/check-python.sh")
    _ = (root / ".gitignore").write_text("private/\nresults/\nops/ansible/.venv/\n")
    for name in (
        *sources,
        "private/secret.py",
        "results/generated.py",
        "vendor/upstream.py",
        "reference/example.py",
    ):
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        _ = path.write_text('"""Inert fixture source."""\n')
    git = installed_tool("git")
    _ = subprocess.run(  # noqa: S603 -- Initialize only this explicitly owned temporary repository.
        [git, "init", "--quiet", "--", str(root)],
        env=fixture_environment(),
        check=True,
        capture_output=True,
        timeout=10,
    )
    tracked = [name for name in sources if name == "build/tracked.py"]
    _ = subprocess.run(  # noqa: S603 -- Stage fixture files only; never modify the real checkout index.
        [git, "-C", str(root), "add", "--", "vendor/upstream.py", "reference/example.py", *tracked],
        env=fixture_environment(),
        check=True,
        capture_output=True,
        timeout=10,
    )
    environment = (
        parent / "external controller environment"
        if external_environment
        else root / "ops/ansible/.venv"
    )
    (environment / "bin").mkdir(parents=True)
    dispatcher = parent / "fake checker.py"
    _ = dispatcher.write_text(FAKE_CHECKER)
    for name in ("python", "ruff", "basedpyright"):
        tool = environment / "bin" / name
        command = " ".join(shlex.quote(value) for value in (sys.executable, str(dispatcher), name))
        _ = tool.write_text("#!/bin/sh\nexec " + command + ' "$@"\n')
        tool.chmod(0o755)
    scratch = parent / "scratch"
    scratch.mkdir()
    return GateFixture(root, parent, environment, parent / "events.jsonl", scratch)


class PythonGateTests(unittest.TestCase):
    """Prove source discovery, explicit tool selection and fail-closed gate sequencing."""

    def test_gate_selects_every_owned_root_untracked_source_and_stub(self) -> None:
        """Keep build scripts, host code, tests and stubs in all three quality stages."""
        fixture = gate_fixture(self)
        result = fixture.run()
        self.assertEqual(result.status, 0, result.stderr)
        self.assertEqual([event.tool for event in result.events], ["ruff", "ruff", "basedpyright"])
        expected = set(FIXTURE_SOURCES)
        self.assertEqual(set(result.events[0].args[2:]), expected)
        self.assertEqual(set(result.events[1].args[3:]), expected)
        self.assertEqual(set(result.events[2].args[2:]), expected)
        self.assertEqual(result.events[0].args[:2], ("check", "--"))
        self.assertEqual(result.events[1].args[:3], ("format", "--check", "--"))
        self.assertEqual(
            result.events[2].args[:2], ("--pythonpath", str(fixture.environment / "bin/python"))
        )
        self.assertTrue(all(event.cwd == fixture.root for event in result.events))
        self.assertEqual(list(fixture.scratch.iterdir()), [])

    def test_explicit_environment_and_paths_with_spaces_do_not_use_default_tools(self) -> None:
        """Bind type analysis to the selected external environment from any working directory."""
        fixture = gate_fixture(self, external_environment=True)
        result = fixture.run(override=fixture.environment)
        self.assertEqual(result.status, 0, result.stderr)
        self.assertFalse((fixture.root / "ops/ansible/.venv").exists())
        self.assertEqual(result.events[-1].args[1], str(fixture.environment / "bin/python"))

    def test_new_python_root_fails_instead_of_silently_escaping_checks(self) -> None:
        """Require an explicit policy update before new source roots become eligible."""
        fixture = gate_fixture(self, sources=(*FIXTURE_SOURCES, "new-owned-root/module.py"))
        result = fixture.run()
        self.assertNotEqual(result.status, 0)
        self.assertIn("Python source outside the checked roots", result.stderr)
        self.assertEqual(result.events, ())
        self.assertEqual(list(fixture.scratch.iterdir()), [])

    def test_missing_checker_executable_stops_before_any_stage(self) -> None:
        """Fail closed when any selected checker is not installed and executable."""
        for name in ("python", "ruff", "basedpyright"):
            with self.subTest(name=name):
                fixture = gate_fixture(self)
                (fixture.environment / "bin" / name).chmod(0o644)
                result = fixture.run()
                self.assertNotEqual(result.status, 0)
                self.assertIn("Missing " + name, result.stderr)
                self.assertEqual(result.events, ())

    def test_an_invalid_explicit_environment_never_falls_back_to_default(self) -> None:
        """Respect a failed explicit environment choice rather than selecting another one."""
        fixture = gate_fixture(self)
        result = fixture.run(override=fixture.temporary / "absent environment")
        self.assertNotEqual(result.status, 0)
        self.assertIn("Missing python", result.stderr)
        self.assertEqual(result.events, ())

    def test_empty_owned_source_set_is_not_a_successful_quality_run(self) -> None:
        """Reject discovery that contains only ignored or third-party Python files."""
        fixture = gate_fixture(self, sources=())
        result = fixture.run()
        self.assertNotEqual(result.status, 0)
        self.assertIn("No maintained Python sources found", result.stderr)
        self.assertEqual(result.events, ())

    def test_failed_stage_preserves_its_status_and_prevents_later_checks(self) -> None:
        """Propagate the original lint, formatting or type-check exit status without retry."""
        for count, stage in enumerate(("lint", "format", "types"), start=1):
            with self.subTest(stage=stage):
                fixture = gate_fixture(self)
                result = fixture.run(fail_stage=stage)
                self.assertEqual(result.status, FAILURE_STATUS, result.stderr)
                self.assertEqual(len(result.events), count)
                self.assertEqual(list(fixture.scratch.iterdir()), [])


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
            line.strip()
            for line in (ROOT / "build/python-requirements.txt").read_text().splitlines()
            if line.strip() and not line.startswith("#")
        }
        self.assertIn("-r ../ops/ansible/requirements.txt", lines)
        for name in ("ruff", "basedpyright", "types-PyYAML"):
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
            step for step in steps if "build/check-python.sh" in string(step.get("run", ""))
        )
        self.assertLess(steps.index(install), steps.index(gate))
        command = string(gate, "run")
        self.assertLess(
            command.index("build/check-python.sh"), command.index("python -m unittest discover")
        )
        self.assertEqual(
            string(gate, "env", "PYTHON_CHECK_ENV"), "${{ runner.temp }}/ansible-controller"
        )
        for scope in (job, gate):
            self.assertNotIn("continue-on-error", scope)
            self.assertNotIn("if", scope)


if __name__ == "__main__":
    _ = unittest.main()
