"""Protect CI trust boundaries, gate completeness and release evidence selection."""

from __future__ import annotations

import unittest
from pathlib import Path
from typing import TYPE_CHECKING

from test_support import array, at, obj, objects, string, strings, yaml_value

if TYPE_CHECKING:
    from release_json import JsonObject

ROOT = Path(__file__).resolve().parents[3]
GATES = {
    "automation",
    "rust-lint",
    "rust-test",
    "native-dtls",
    "browser",
    "web",
    "deployment",
    "security",
    "codeql",
}


def workflow(name: str) -> JsonObject:
    """Read workflow scalars without YAML 1.1 boolean coercion."""
    return obj(yaml_value((ROOT / ".github/workflows" / name).read_text(), scalars_as_strings=True))


class SecurityWorkflowTests(unittest.TestCase):
    """Require actual dependencies and restricted credentials, not just job names."""

    def test_aggregate_rejects_missing_skipped_and_failed_jobs(self) -> None:
        """Every maintained correctness/security job must be in the always-run aggregate."""
        ci = workflow("ci.yml")
        gate = obj(ci, "jobs", "required")
        self.assertEqual(gate["if"], "${{ always() }}")
        self.assertEqual(set(strings(gate, "needs")), GATES)
        self.assertEqual(set(obj(ci, "jobs")) - {"required", "release-security"}, GATES)
        steps = objects(gate, "steps")
        self.assertEqual(len(steps), 1)
        self.assertEqual(at(steps[0], "env", "GATE_RESULTS"), "${{ toJSON(needs) }}")
        command = string(steps[0], "run")
        self.assertIn("set(results) != expected", command)
        self.assertIn('item["result"] != "success"', command)
        self.assertNotIn("continue-on-error", gate)

    def test_signing_only_follows_all_main_push_gates(self) -> None:
        """PRs and partial CI runs cannot request a signing identity."""
        ci = workflow("ci.yml")
        signer = obj(ci, "jobs", "release-security")
        self.assertEqual(
            signer["if"], "${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}"
        )
        self.assertEqual(signer["needs"], ["required"])
        self.assertEqual(signer["runs-on"], "ubuntu-24.04")
        self.assertEqual(
            signer["permissions"],
            {"contents": "read", "id-token": "write", "attestations": "write"},
        )
        for name, value in obj(ci, "jobs").items():
            if name != "release-security":
                self.assertNotIn("id-token", obj(obj(value).get("permissions", {})))
                self.assertNotIn("attestations", obj(obj(value).get("permissions", {})))
        steps = objects(signer, "steps")
        downloads = [
            step
            for step in steps
            if string(step.get("uses", "")).startswith("actions/download-artifact@")
        ]
        self.assertEqual(len(downloads), 2)
        for step in downloads:
            self.assertNotIn("run-id", obj(step, "with"))
            self.assertNotIn("github-token", obj(step, "with"))
            self.assertNotIn("repository", obj(step, "with"))
        attest = next(step for step in steps if step.get("id") == "attest")
        self.assertRegex(string(attest, "uses"), r"^actions/attest@[a-f0-9]{40}$")
        self.assertEqual(
            string(attest, "with", "subject-path").splitlines(),
            [
                "${{ runner.temp }}/signed-release/image.tar",
                "${{ runner.temp }}/signed-release/sbom.spdx.json",
            ],
        )
        self.assertEqual(at(attest, "with", "create-storage-record"), "false")
        uploads = [
            step
            for step in steps
            if string(step.get("uses", "")).startswith("actions/upload-artifact@")
        ]
        self.assertEqual(len(uploads), 1)
        expected = {
            "image.tar",
            "release.json",
            "outcome.json",
            "source.json",
            "sbom.spdx.json",
            "image-security.json",
            "release-predicate.json",
            "release-attestation.jsonl",
        }
        self.assertEqual(
            set(string(uploads[0], "with", "path").splitlines()),
            {"${{ runner.temp }}/signed-release/" + name for name in expected},
        )

    def test_reusable_checks_share_revision_and_do_not_inherit_secrets(self) -> None:
        """Called analysis uses the caller checkout and independent concurrency groups."""
        ci = workflow("ci.yml")
        for name in ("security", "codeql"):
            caller = obj(ci, "jobs", name)
            self.assertEqual(caller["uses"], f"./.github/workflows/{name}.yml")
            self.assertNotIn("secrets", caller)
            called = workflow(name + ".yml")
            self.assertIn("workflow_call", obj(called, "on"))
            self.assertNotIn("push", obj(called, "on"))
            self.assertNotIn("pull_request", obj(called, "on"))
            self.assertTrue(string(called, "concurrency", "group").startswith(name + "-"))
            self.assertTrue(array(called, "on", "schedule"))

    def test_codeql_requires_manual_observed_native_compilation(self) -> None:
        """A source-only or restored compilation cannot satisfy native CodeQL coverage."""
        native = obj(workflow("codeql.yml"), "jobs", "native-analysis")
        steps = objects(native, "steps")
        init = next(step for step in steps if step.get("id") == "init")
        self.assertEqual(at(init, "with", "build-mode"), "manual")
        build = next(
            step
            for step in steps
            if step.get("name") == "Compile the actual worker under CodeQL tracing"
        )
        command = string(build, "run")
        self.assertIn("libmediasoup-worker", command)
        self.assertNotIn("docker", command)
        self.assertNotIn("env -i", command)
        self.assertLess(steps.index(init), steps.index(build))
        self.assertTrue(
            any("build/security_codeql.py" in string(step.get("run", "")) for step in steps)
        )
        self.assertFalse(any("cache@" in string(step.get("uses", "")) for step in steps))

    def test_all_workflows_use_unprivileged_pr_events_and_pinned_actions(self) -> None:
        """Pin external action code and prohibit privileged PR execution for all jobs."""
        for path in (ROOT / ".github/workflows").glob("*.yml"):
            value = workflow(path.name)
            with self.subTest(workflow=path.name):
                self.assertNotIn("pull_request_target", obj(value, "on"))
                for job in obj(value, "jobs").values():
                    for step in objects(obj(job).get("steps", [])):
                        uses = string(step.get("uses", ""))
                        if uses and not uses.startswith(("./", "$/")):
                            self.assertRegex(
                                uses, r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_./-]+@[a-f0-9]{40}$"
                            )

    def test_build_caches_cannot_fall_back_across_trust_or_architecture(self) -> None:
        """PR build artifacts remain outside the main compiler/layer cache namespace."""
        composite = obj(
            yaml_value(
                (ROOT / ".github/actions/native-toolchain/action.yml").read_text(),
                scalars_as_strings=True,
            )
        )
        sources = [objects(composite, "runs", "steps")]
        sources.extend(
            objects(job.get("steps", []))
            for job in (
                obj(workflow("ci.yml"), "jobs", "deployment"),
                obj(workflow("load-generator-image.yml"), "jobs", "image"),
            )
        )
        for steps in sources:
            for step in steps:
                if not string(step.get("uses", "")).startswith(
                    ("actions/cache", "Swatinem/rust-cache")
                ):
                    continue
                config = obj(step, "with")
                key = string(config.get("prefix-key", config.get("key", "")))
                self.assertIn("github.event_name != 'pull_request'", key)
                self.assertIn("'main' || 'untrusted'", key)
                self.assertIn("runner.arch", key)
                if "restore-keys" in config:
                    self.assertIn("'main' || 'untrusted'", string(config, "restore-keys"))


if __name__ == "__main__":
    _ = unittest.main()
