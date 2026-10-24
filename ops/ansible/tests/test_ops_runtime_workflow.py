"""Protect manual Ops authority and execute its real request writer in owned fixtures."""

from __future__ import annotations

import hashlib
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from typing import TYPE_CHECKING, cast, override

from test_support import ROOT, obj, objects, string, strings, yaml_value

if TYPE_CHECKING:
    from release_json import JsonObject, JsonValue

CHECKOUT = "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1"
UPLOAD = "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a"
GUARD = (
    "${{ github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/main' "
    "&& github.repository == 'swiftraccoon/simplestChat' }}"
)
MAX_ID = 9007199254740991
IDENTIFIERS = ("OPS_RUN_ID", "OPS_RUN_ATTEMPT", "OPS_ARTIFACT_ID")


def workflow_steps() -> list[JsonObject]:
    """Keep every test on the current workflow, with YAML 1.2 scalar spelling."""
    workflow = yaml_value(
        (ROOT / ".github/workflows/ops-runtime.yml").read_text(), scalars_as_strings=True
    )
    return objects(workflow, "jobs", "ops-runtime", "steps")


def request_writer() -> str:
    """Extract the actual sole Python heredoc, without rewriting its behavior."""
    selected = [step for step in workflow_steps() if step.get("id") == "request"]
    if len(selected) != 1:
        message = "one inline request writer is required"
        raise AssertionError(message)
    lines = string(selected[0], "run").splitlines()
    if not lines or lines[0] != "python3 - <<'PY'" or lines[-1] != "PY":
        message = "the reviewed Python heredoc boundary changed"
        raise AssertionError(message)
    return "\n".join(lines[1:-1]) + "\n"


class OpsRuntimeWorkflowTests(unittest.TestCase):
    """A manual proof job must not become a signer or a mandatory production gate."""

    def test_manual_main_only_readonly_scope_has_no_caller_selected_checkout(self) -> None:
        """Inputs select an artifact identity, while the event selects the source checkout."""
        document = obj(
            yaml_value(
                (ROOT / ".github/workflows/ops-runtime.yml").read_text(),
                scalars_as_strings=True,
            )
        )
        self.assertEqual(set(obj(document, "on")), {"workflow_dispatch"})
        inputs = obj(document, "on", "workflow_dispatch", "inputs")
        self.assertEqual(set(inputs), {"run_id", "run_attempt", "artifact_id"})
        for name in inputs:
            self.assertEqual(obj(inputs, name)["type"], "string")
            self.assertEqual(obj(inputs, name)["required"], "true")
        self.assertEqual(document["permissions"], {"contents": "read", "actions": "read"})
        self.assertNotIn("env", document)
        self.assertEqual(set(obj(document, "jobs")), {"ops-runtime"})
        job = obj(document, "jobs", "ops-runtime")
        self.assertEqual(job["if"], GUARD)
        self.assertEqual(job["runs-on"], "ubuntu-24.04")
        self.assertNotIn("permissions", job)
        self.assertNotIn("env", job)
        self.assertNotIn("needs", job)
        steps = objects(job, "steps")
        self.assertEqual(steps[0]["uses"], CHECKOUT)
        self.assertEqual(steps[0]["with"], {"persist-credentials": "false"})
        for step in steps:
            self.assertNotIn("continue-on-error", step)
            self.assertNotIn("secrets", step)
            self.assertNotIn("${{", string(step.get("run", "")))
            if step.get("uses") != UPLOAD:
                self.assertNotIn("if", step)
        canonical = yaml_value(
            (ROOT / ".github/workflows/ci.yml").read_text(), scalars_as_strings=True
        )
        self.assertNotIn("ops-runtime", strings(canonical, "jobs", "required", "needs"))

    def test_api_credential_is_only_in_the_post_bootstrap_runtime_leaf(self) -> None:
        """Compilation and authenticated tool installation finish before token exposure."""
        steps = workflow_steps()
        token_steps = [
            index for index, step in enumerate(steps) if "GH_TOKEN" in obj(step.get("env", {}))
        ]
        self.assertEqual(len(token_steps), 1)
        runtime = token_steps[0]
        self.assertEqual(
            steps[runtime]["env"],
            {
                "GH_TOKEN": "${{ github.token }}",
                "OPS_REQUEST_SHA256": "${{ steps.request.outputs.sha256 }}",
            },
        )
        self.assertEqual(
            string(steps[runtime], "run").splitlines(),
            [
                "target/debug/xtask ops-artifact runtime \\",
                '  --request "$RUNNER_TEMP/ops-runtime-request.json" \\',
                '  --request-sha256 "$OPS_REQUEST_SHA256"',
            ],
        )
        compile_steps = [
            index
            for index, step in enumerate(steps)
            if step.get("run") == "cargo build --locked -p xtask --bin xtask"
        ]
        install_steps = [
            index
            for index, step in enumerate(steps)
            if "security_tools.py install --tools gh" in string(step.get("run", ""))
        ]
        self.assertEqual(len(compile_steps), 1)
        self.assertEqual(len(install_steps), 1)
        self.assertLess(compile_steps[0], runtime)
        self.assertLess(install_steps[0], runtime)
        bootstrap = string(steps[install_steps[0]], "run")
        self.assertLess(
            bootstrap.index("security_tools.py install --tools gh"),
            bootstrap.index("security_tools.py path gh --directory"),
        )
        self.assertIn('test "$ops_gh_executable" = "$RUNNER_TEMP/ops-gh/bin/gh"', bootstrap)
        for index, step in enumerate(steps):
            env = obj(step.get("env", {}))
            self.assertNotIn("GITHUB_TOKEN", env)
            if index != runtime:
                self.assertNotIn("GH_TOKEN", env)
        request = next(step for step in steps if step.get("id") == "request")
        self.assertLess(steps.index(request), runtime)
        self.assertEqual(
            request["env"],
            {
                "OPS_RUN_ID": "${{ inputs.run_id }}",
                "OPS_RUN_ATTEMPT": "${{ inputs.run_attempt }}",
                "OPS_ARTIFACT_ID": "${{ inputs.artifact_id }}",
            },
        )

    def test_watchdogs_leave_retirement_time_and_only_success_uploads_the_summary(self) -> None:
        """Private intermediate artifacts and failed runs cannot become public evidence."""
        document = yaml_value(
            (ROOT / ".github/workflows/ops-runtime.yml").read_text(), scalars_as_strings=True
        )
        job = obj(document, "jobs", "ops-runtime")
        steps = objects(job, "steps")
        runtime = next(step for step in steps if "GH_TOKEN" in obj(step.get("env", {})))
        self.assertEqual(job["timeout-minutes"], "90")
        self.assertEqual(runtime["timeout-minutes"], "75")
        plan = (ROOT / "xtask/src/ci/ops_artifact/runtime/plan.rs").read_text()
        entry = [
            match.group(1)
            for match in re.finditer(
                r"const ENTRY: Duration = Duration::from_secs\(([0-9]+)\)", plan
            )
        ]
        self.assertEqual(entry, ["4200"])
        self.assertLess(int(entry[0]), int(string(runtime, "timeout-minutes")) * 60)
        self.assertLess(
            int(string(runtime, "timeout-minutes")), int(string(job, "timeout-minutes"))
        )
        uploads = [step for step in steps if step.get("uses") == UPLOAD]
        self.assertEqual(uploads, [steps[-1]])
        self.assertEqual(uploads[0]["if"], "${{ success() }}")
        self.assertEqual(
            uploads[0]["with"],
            {
                "name": "ops-runtime-summary",
                "path": "${{ runner.temp }}/ops-runtime/summary.json",
                "if-no-files-found": "error",
                "retention-days": "14",
            },
        )


class OpsRuntimeRequestWriterTests(unittest.TestCase):
    """Run only the reviewed inline Python against temporary source and output files."""

    root: Path = Path()
    runner: Path = Path()
    request: Path = Path()
    output: Path = Path()
    environment: dict[str, str] | None = None

    @override
    def setUp(self) -> None:
        """No credentials or ambient environment reach this writer-only process."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        _ = (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "9.8.7"\n')
        self.runner = self.root / "runner"
        self.runner.mkdir(mode=0o700)
        self.request = self.runner / "ops-runtime-request.json"
        self.output = self.root / "github-output"
        self.output.touch(mode=0o600)
        self.environment = {
            "PATH": os.defpath,
            "RUNNER_TEMP": str(self.runner),
            "GITHUB_OUTPUT": str(self.output),
            "GITHUB_SHA": "a" * 40,
            "OPS_RUN_ID": "1",
            "OPS_RUN_ATTEMPT": str(MAX_ID),
            "OPS_ARTIFACT_ID": "42",
        }

    def invoke(self, overrides: dict[str, str] | None = None) -> subprocess.CompletedProcess[bytes]:
        """Execute the actual trusted writer with a permissive umask and a five-second cap."""
        environment = self.environment
        if environment is None:
            message = "The owned writer fixture was not initialized."
            raise AssertionError(message)
        return subprocess.run(  # noqa: S603 -- Fixed Python and reviewed inline writer; owned cwd/outputs, no credentials/network.
            [sys.executable, "-I", "-B", "-c", request_writer()],
            cwd=self.root,
            env=environment | (overrides or {}),
            capture_output=True,
            timeout=5,
            check=False,
            umask=0,
        )

    def test_original_request_sha_private_mode_and_source_identity_are_pinned(self) -> None:
        """The emitted SHA binds the exact closed bytes, including their final newline."""
        completed = self.invoke()
        self.assertEqual(completed.returncode, 0, completed.stderr)
        original = self.request.read_bytes()
        self.assertTrue(original.endswith(b"\n"))
        self.assertEqual(
            self.output.read_text(), "sha256=" + hashlib.sha256(original).hexdigest() + "\n"
        )
        self.assertEqual(stat.S_IMODE(self.request.stat().st_mode), 0o600)
        self.assertEqual(self.request.stat().st_uid, os.getuid())
        value = obj(cast("JsonValue", json.loads(original)))
        self.assertEqual(
            value,
            {
                "schemaVersion": 1,
                "selection": {
                    "repository": "swiftraccoon/simplestChat",
                    "revision": "a" * 40,
                    "packageVersion": "9.8.7",
                    "target": "x86_64-unknown-linux-musl",
                    "protocolVersion": 1,
                    "buildRunId": 1,
                    "runAttempt": MAX_ID,
                },
                "artifactId": 42,
                "output": str(self.runner / "ops-runtime"),
                "authentication": "gh-token",
            },
        )
        selection = obj(value, "selection")
        for number in (
            value["schemaVersion"],
            selection["protocolVersion"],
            selection["buildRunId"],
            selection["runAttempt"],
            value["artifactId"],
        ):
            self.assertIs(type(number), int)
        self.assertEqual(completed.stdout, b"")

    def test_identifier_failures_precede_request_or_output_publication(self) -> None:
        """Ambiguous numbers, zero, overflow and non-ASCII forms never select an artifact."""
        for name in IDENTIFIERS:
            for raw in (
                "",
                "0",
                "-1",
                "+1",
                "01",
                "1.0",
                "1e3",
                " 1",
                "1\n",
                "a",
                str(MAX_ID + 1),
                "9" * 17,
                "\uff19",
            ):
                with self.subTest(name=name, raw=raw):
                    completed = self.invoke({name: raw})
                    self.assertNotEqual(completed.returncode, 0)
                    self.assertEqual(completed.stderr, b"Invalid Ops artifact identifier.\n")
                    self.assertFalse(self.request.exists())
                    self.assertEqual(self.output.read_bytes(), b"")

    def test_all_identifier_fields_accept_the_exact_upper_boundary(self) -> None:
        """The full safe-integer boundary remains usable as an integer in every role."""
        completed = self.invoke(dict.fromkeys(IDENTIFIERS, str(MAX_ID)))
        self.assertEqual(completed.returncode, 0, completed.stderr)
        value = obj(cast("JsonValue", json.loads(self.request.read_bytes())))
        self.assertEqual(obj(value, "selection")["buildRunId"], MAX_ID)
        self.assertEqual(obj(value, "selection")["runAttempt"], MAX_ID)
        self.assertEqual(value["artifactId"], MAX_ID)

    def test_noncanonical_source_revision_never_writes_a_request(self) -> None:
        """The source identity comes only from a canonical event SHA."""
        for revision in ("", "a" * 39, "a" * 41, "A" * 40, "g" * 40, "a" * 40 + "\n"):
            with self.subTest(revision=revision):
                completed = self.invoke({"GITHUB_SHA": revision})
                self.assertNotEqual(completed.returncode, 0)
                self.assertEqual(completed.stderr, b"Invalid Ops source revision.\n")
                self.assertFalse(self.request.exists())
                self.assertEqual(self.output.read_bytes(), b"")

    def test_exclusive_creation_preserves_regular_symlink_and_fifo_collisions(self) -> None:
        """No existing request path or a symlink target can be adopted or overwritten."""
        target = self.root / "keep"
        _ = target.write_bytes(b"owned sentinel")
        for kind in ("regular", "symlink", "fifo"):
            with self.subTest(kind=kind):
                if kind == "regular":
                    _ = self.request.write_bytes(b"pending request")
                elif kind == "symlink":
                    self.request.symlink_to(target)
                else:
                    os.mkfifo(self.request, 0o600)
                before = self.request.lstat()
                completed = self.invoke()
                self.assertNotEqual(completed.returncode, 0)
                after = self.request.lstat()
                self.assertEqual(
                    (
                        before.st_dev,
                        before.st_ino,
                        before.st_mode,
                        before.st_size,
                        before.st_mtime_ns,
                    ),
                    (after.st_dev, after.st_ino, after.st_mode, after.st_size, after.st_mtime_ns),
                )
                self.assertEqual(target.read_bytes(), b"owned sentinel")
                self.assertEqual(self.output.read_bytes(), b"")
                self.request.unlink()
