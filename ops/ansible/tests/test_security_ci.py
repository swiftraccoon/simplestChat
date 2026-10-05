"""Protect CI trust boundaries, gate completeness and release evidence selection."""

from __future__ import annotations

import hashlib
import json
import re
import shlex
import subprocess
import tempfile
import unittest
from pathlib import Path
from typing import TYPE_CHECKING, cast

import yaml
from test_support import array, at, obj, objects, string, strings, yaml_value
from yaml.nodes import MappingNode, Node, SequenceNode

# isort: split
import security_policy

if TYPE_CHECKING:
    from collections.abc import Callable

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

    def reviewed_feature(self, text: str, components: list[str | int]) -> str:
        """Preserve scalar features and exact source spelling for reviewed YAML blocks."""
        self.assertLessEqual(len(text.encode()), security_policy.MAX_POLICY)
        document = yaml_value(text, scalars_as_strings=True)
        selected = at(document, *components)
        if isinstance(selected, str):
            return selected
        self.assertIsInstance(selected, dict)
        compose = cast("Callable[[str], Node | None]", yaml.compose)
        node = compose(text)
        start = 0
        for component in components:
            if isinstance(component, int):
                self.assertIsInstance(node, SequenceNode)
                node = cast("list[Node]", cast("SequenceNode", node).value)[component]
                start = node.start_mark.index
            else:
                self.assertIsInstance(node, MappingNode)
                matches = [
                    (key, value)
                    for key, value in cast(
                        "list[tuple[Node, Node]]", cast("MappingNode", node).value
                    )
                    if cast("object", key.value) == component
                ]
                self.assertEqual(len(matches), 1)
                key, node = matches[0]
                start = key.start_mark.index
        self.assertIsInstance(node, MappingNode)
        return text[start : cast("MappingNode", node).end_mark.index].rstrip()

    def test_reviewed_mapping_preserves_source_bytes_and_detects_nested_changes(self) -> None:
        """A reviewed trigger includes its key and nested values, without a final newline."""
        feature = (
            "on:\n  workflow_run:\n    workflows: [CI]\n"
            "    types: [completed]\n    branches: [main]"
        )
        text = "name: Cache retention\n\n" + feature + "\n\npermissions:\n  contents: read\n"
        self.assertEqual(self.reviewed_feature(text, ["on"]), feature)
        self.assertNotEqual(
            self.reviewed_feature(text.replace("[main]", "[other]"), ["on"]), feature
        )
        action = "owner/action@" + "a" * 40
        self.assertEqual(
            self.reviewed_feature(
                "steps:\n  - uses: " + action + " # pinned\n", ["steps", 0, "uses"]
            ),
            action,
        )

    def test_reviewed_workflow_locations_still_identify_the_same_feature(self) -> None:
        """Moving a reviewed action must update its exact review location before CI runs."""
        reviews = [
            entry for entry in security_policy.read_exceptions() if entry.scanner == "zizmor"
        ]
        self.assertTrue(reviews)
        for review in reviews:
            with self.subTest(scope=review.scope):
                name, route = review.scope.split("#", 1)
                text = (ROOT / name).read_text()
                components = [int(part) if part.isdecimal() else part for part in route.split("/")]
                feature = self.reviewed_feature(text, components)
                self.assertEqual(
                    review.fingerprint.split(":", 1)[1],
                    hashlib.sha256(feature.encode()).hexdigest(),
                )

    def test_aggregate_rejects_missing_skipped_and_failed_jobs(self) -> None:
        """Every maintained correctness/security job must be in the always-run aggregate."""
        ci = workflow("ci.yml")
        gate = obj(ci, "jobs", "required")
        self.assertEqual(gate["if"], "${{ always() }}")
        self.assertEqual(set(strings(gate, "needs")), GATES)
        self.assertEqual(set(obj(ci, "jobs")) - {"required", "release-security"}, GATES)
        steps = objects(gate, "steps")
        self.assertEqual(at(steps[0], "env", "GATE_RESULTS"), "${{ toJSON(needs) }}")
        command = string(steps[0], "run")
        self.assertIn("set(results) != expected", command)
        self.assertIn('item["result"] != "success"', command)
        self.assertNotIn("continue-on-error", gate)

    def test_aggregate_checks_stored_codeql_health_before_signing(self) -> None:
        """Green jobs do not certify failed ingestion; fork PRs use the same read-only gate."""
        gate = obj(workflow("ci.yml"), "jobs", "required")
        self.assertEqual(gate["permissions"], {"contents": "read", "security-events": "read"})
        self.assertEqual(gate["timeout-minutes"], "7")
        steps = objects(gate, "steps")
        self.assertTrue(string(steps[1], "uses").startswith("actions/checkout@"))
        self.assertEqual(at(steps[1], "with", "persist-credentials"), "false")
        health = next(
            step
            for step in steps
            if "build/security_codeql_triage.py health" in string(step.get("run", ""))
        )
        self.assertEqual(health["env"], {"GH_TOKEN": "${{ github.token }}"})
        self.assertEqual(health["if"], "${{ !env.ACT }}")
        self.assertNotIn("continue-on-error", health)
        command = string(health, "run")
        self.assertIn("python3 build/security_codeql_triage.py health", command)
        for argument in (
            '--repository "$GITHUB_REPOSITORY"',
            '--revision "$GITHUB_SHA"',
            '--ref "$GITHUB_REF"',
            '--output "$RUNNER_TEMP/codeql-analysis-health"',
        ):
            self.assertIn(argument, command)
        upload = next(
            step
            for step in steps
            if string(step.get("uses", "")).startswith("actions/upload-artifact@")
        )
        self.assertEqual(upload["if"], "${{ always() && !env.ACT }}")
        self.assertEqual(
            string(upload, "with", "path").splitlines(),
            [
                "${{ runner.temp }}/codeql-analysis-health/report.json",
                "${{ runner.temp }}/codeql-analysis-health/failure.json",
            ],
        )

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
                "${{ runner.temp }}/signed-release/runtime-proof.json",
                "${{ runner.temp }}/signed-release/vex.openvex.json",
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
            "runtime-proof.json",
            "vex.openvex.json",
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

    def test_codeql_advisory_runs_are_scheduled_or_manual_and_security_remains_reusable(
        self,
    ) -> None:
        """Direct-main development retains all security scans without duplicate advisory PR jobs."""
        codeql = workflow("codeql.yml")
        self.assertEqual(set(obj(codeql, "on")), {"workflow_call", "schedule", "workflow_dispatch"})
        gate = obj(codeql, "on", "workflow_call", "inputs", "security_gate")
        self.assertEqual(gate["type"], "boolean")
        self.assertEqual(gate["default"], "true")
        self.assertNotIn("with", obj(workflow("ci.yml"), "jobs", "codeql"))
        self.assertNotIn("inputs", obj(obj(codeql, "on").get("workflow_dispatch") or {}))
        self.assertEqual(set(obj(codeql, "jobs")), {"source-analysis"})
        self.assertEqual(
            strings(codeql, "jobs", "source-analysis", "strategy", "matrix", "language"),
            ["actions", "javascript-typescript", "python", "rust"],
        )
        self.assertEqual(
            string(codeql, "concurrency", "group"),
            "codeql-${{ github.workflow }}-${{ github.event.pull_request.number || github.ref }}",
        )

    def test_codeql_routes_called_security_and_standalone_advisory_suites(self) -> None:
        """Called and standalone modes select matching query/category pairs."""
        for name, job in [
            ("source-analysis", obj(workflow("codeql.yml"), "jobs", "source-analysis"))
        ]:
            with self.subTest(job=name):
                steps = objects(job, "steps")
                init = next(
                    step for step in steps if "codeql-action/init@" in string(step.get("uses", ""))
                )
                analyze = next(
                    step
                    for step in steps
                    if "codeql-action/analyze@" in string(step.get("uses", ""))
                )
                self.assertEqual(
                    string(init, "with", "queries"),
                    "${{ inputs.security_gate && 'security-extended' || 'security-and-quality' }}",
                )
                language = "c-cpp" if name == "native-analysis" else "${{ matrix.language }}"
                self.assertEqual(
                    string(analyze, "with", "category"),
                    "/language:"
                    + language
                    + "/${{ inputs.security_gate && 'security' || 'quality-advisory' }}",
                )
                self.assertEqual(init["if"], "${{ !env.ACT && matrix.language != 'rust' }}")
                self.assertEqual(analyze["if"], "${{ !env.ACT && matrix.language != 'rust' }}")

    def test_optional_native_codeql_retains_real_worker_build(self) -> None:
        """The local opt-in keeps native extraction available without a hosted job."""
        command = (ROOT / "build/codeql-native-build.sh").read_text()
        self.assertIn("libmediasoup-worker", command)
        self.assertIn('mktemp -d "$project_root/target/codeql-worker.XXXXXXXX"', command)
        self.assertIn("build/security_codeql_resources.py", command)
        self.assertNotIn("docker", command)
        setup = "python3 -m invoke --search-root vendor/mediasoup-sys-0.19.0 setup"
        generator = '-j "$MEDIASOUP_BUILD_JOBS" flatbuffers-generator'
        worker = "python3 -m invoke --search-root vendor/mediasoup-sys-0.19.0 libmediasoup-worker"
        self.assertLess(command.index(setup), command.index(generator))
        self.assertLess(command.index(generator), command.index(worker))
        self.assertIn('NINJA="$MEDIASOUP_OUT_DIR/pip_meson_ninja/bin/ninja"', command)
        self.assertIn(
            '"$MEDIASOUP_OUT_DIR/pip_meson_ninja/bin/meson" compile -C "$BUILD_DIR"', command
        )
        self.assertIn("export MEDIASOUP_BUILDTYPE=Release", command)
        self.assertNotIn("-Doptimization", command)
        self.assertIn("CodeQL FlatBuffers generator:", command)
        self.assertIn("CodeQL worker library:", command)

    def test_codeql_excludes_vendor_for_every_automated_entrypoint(self) -> None:
        """Called, scheduled and dispatched analysis share the same first-party scope."""
        codeql = workflow("codeql.yml")
        self.assertEqual(set(obj(codeql, "jobs")), {"source-analysis"})
        text = (ROOT / ".github/workflows/codeql.yml").read_text()
        self.assertNotIn("--include-vendor", text)
        self.assertNotIn("c-cpp", text)
        steps = objects(codeql, "jobs", "source-analysis", "steps")
        initialize = next(
            step for step in steps if "codeql-action/init@" in string(step.get("uses", ""))
        )
        self.assertEqual(at(initialize, "with", "config-file"), "security/codeql/first-party.yml")
        config = workflow("../../security/codeql/first-party.yml")
        self.assertEqual(strings(config, "paths-ignore"), ["vendor/**"])

    def test_codeql_cache_publication_preserves_policy_failure_and_rejects_stale_markers(
        self,
    ) -> None:
        """Execute the Rust workflow wrapper with failed commands and fresh or stale evidence."""
        codeql = workflow("codeql.yml")
        for job, language, runner_id in (("source-analysis", "rust", "source-analysis"),):
            steps = objects(codeql, "jobs", job, "steps")
            runner = next(step for step in steps if step.get("id") == runner_id)
            save = next(
                step
                for step in steps
                if string(step.get("uses", "")).startswith("actions/cache/save@")
            )
            condition = string(save, "if")
            self.assertIn("always() && !cancelled()", condition)
            self.assertIn(f"steps.{runner_id}.outputs.database-cache-ready == 'true'", condition)
            self.assertIn("outputs.cache-hit != 'true'", condition)
            for fresh in (False, True):
                with (
                    self.subTest(language=language, fresh=fresh),
                    tempfile.TemporaryDirectory(prefix="codeql-step-", dir=ROOT / "results") as tmp,
                ):
                    directory = Path(tmp)
                    (directory / "codeql-local").mkdir()
                    marker = directory / "codeql-local" / (language + "-cache-ready.json")
                    _ = marker.write_text("stale")
                    output = directory / "outputs"
                    script = (
                        'python3() { if [[ "$TEST_FRESH" == true ]]; then '
                        + ': > "$RUNNER_TEMP/codeql-local/$CODEQL_LANGUAGE-cache-ready.json"; '
                        + "fi; return 7; }\n"
                        + string(runner, "run")
                    )
                    result = subprocess.run(  # noqa: S603 -- Owned fixture of the checked wrapper.
                        ["/bin/bash", "-e", "-c", script],
                        env={
                            "PATH": "/usr/bin:/bin",
                            "RUNNER_TEMP": tmp,
                            "GITHUB_OUTPUT": str(output),
                            "GITHUB_SHA": "a" * 40,
                            "CODEQL_LANGUAGE": language,
                            "CODEQL_SUITE": "security",
                            "OPENSSL_DIR": str(directory / "openssl"),
                            "TEST_FRESH": str(fresh).lower(),
                        },
                        capture_output=True,
                        text=True,
                        check=False,
                        timeout=5,
                    )
                    self.assertEqual(result.returncode, 7, result.stderr)
                    self.assertEqual(marker.exists(), fresh)
                    self.assertEqual(
                        output.read_text() if output.exists() else "",
                        "database-cache-ready=true\n" if fresh else "",
                    )

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

    def test_vendor_security_suites_are_explicit_local_only(self) -> None:
        """No hosted, scheduled or complete-local workflow executes vendor source suites."""
        jobs = obj(workflow("security.yml"), "jobs")
        self.assertEqual(set(jobs), {"security-fast", "security-mutation"})
        for job in jobs.values():
            commands = "\n".join(string(step.get("run", "")) for step in objects(obj(job), "steps"))
            for forbidden in ("native_security.py", "--include-vendor", "--deep-check native"):
                self.assertNotIn(forbidden, commands)

    def test_dependency_reviews_are_shared_by_direct_main_and_local_source_checks(self) -> None:
        """Changed npm, Python and Actions dependencies retain policy checks without a PR job."""
        jobs = obj(workflow("security.yml"), "jobs")
        self.assertNotIn("dependency-review", jobs)
        fast = obj(jobs, "security-fast")
        self.assertNotIn("if", fast)
        gate = next(
            step
            for step in objects(fast, "steps")
            if "build/check-security.sh fast" in string(step.get("run", ""))
        )
        self.assertNotIn("if", gate)
        self.assertEqual(
            at(gate, "env", "SECURITY_BASE"),
            "${{ github.event.pull_request.base.sha || github.event.before }}",
        )
        source = (ROOT / "build/security_check.py").read_text()
        self.assertIn(
            "security_dependency_licenses.check(context, snapshot, base, "
            + "include_vendor=include_vendor)",
            source,
        )
        self.assertIn(
            "security_actions.check(context, snapshot, base, include_vendor=include_vendor)", source
        )
        self.assertIn('vulnerabilities["total"] == 0', source)
        self.assertIn('"pip_audit"', source)

    def test_source_security_jobs_bootstrap_rust_before_running_shared_checks(self) -> None:
        """Fresh local images need the same pinned installer used by native CI jobs."""
        native = obj(
            yaml_value(
                (ROOT / ".github/actions/native-toolchain/action.yml").read_text(),
                scalars_as_strings=True,
            )
        )
        existing = next(
            step
            for step in objects(native, "runs", "steps")
            if string(step.get("uses", "")).startswith("dtolnay/rust-toolchain@")
        )
        jobs = obj(workflow("security.yml"), "jobs")
        for name in ("security-fast",):
            with self.subTest(job=name):
                steps = objects(jobs, name, "steps")
                installers = [
                    step
                    for step in steps
                    if string(step.get("uses", "")).startswith("dtolnay/rust-toolchain@")
                ]
                self.assertEqual(len(installers), 1)
                installer = installers[0]
                self.assertEqual(installer["uses"], existing["uses"])
                self.assertEqual(
                    installer["with"], {"toolchain": at(existing, "with", "toolchain")}
                )
                self.assertNotIn("if", installer)
                self.assertNotIn("continue-on-error", installer)
                gate = next(
                    step
                    for step in steps
                    if "build/check-security.sh" in string(step.get("run", ""))
                )
                self.assertLess(steps.index(installer), steps.index(gate))
                self.assertNotIn(
                    "rustup toolchain install",
                    "\n".join(string(step.get("run", "")) for step in steps),
                )

    def test_local_codeql_runs_full_pinned_analysis_before_recording_success(self) -> None:
        """Local success requires each real language policy; hosted uploads stay on GitHub."""
        codeql = workflow("codeql.yml")
        self.assertEqual(at(codeql, "env", "CODEQL_ACTION_DIFF_INFORMED_QUERIES"), "false")
        self.assertEqual(
            at(codeql, "jobs", "source-analysis", "strategy", "max-parallel"),
            "4",
        )
        for name, job in obj(codeql, "jobs").items():
            with self.subTest(job=name):
                steps = objects(job, "steps")
                local = next(
                    step
                    for step in steps
                    if "build/security_codeql_local.py" in string(step.get("run", ""))
                )
                if name == "native-analysis":
                    self.assertNotIn("if", local)
                else:
                    self.assertEqual(local["if"], "${{ env.ACT || matrix.language == 'rust' }}")
                self.assertNotIn("continue-on-error", local)
                self.assertIn('--revision "$GITHUB_SHA"', string(local, "run"))
                self.assertIn('--suite "$CODEQL_SUITE"', string(local, "run"))
                language = "c-cpp" if name == "native-analysis" else '"$CODEQL_LANGUAGE"'
                self.assertIn("--language " + language, string(local, "run"))
                recorder = next(
                    step
                    for step in steps
                    if "ci-local-evidence.py record" in string(step.get("run", ""))
                )
                self.assertEqual(recorder["if"], "${{ env.ACT }}")
                self.assertLess(steps.index(local), steps.index(recorder))
                if name == "native-analysis":
                    continue  # The shared runner verifies the same pin for both environments.
                hosted_pin = next(
                    step
                    for step in steps
                    if "build/security_codeql_tools.py" in string(step.get("run", ""))
                )
                self.assertEqual(hosted_pin["if"], "${{ !env.ACT && matrix.language != 'rust' }}")
                self.assertIn('--codeql "$CODEQL_BINARY"', string(hosted_pin, "run"))
        for value in (codeql, workflow("security.yml")):
            for job in obj(value, "jobs").values():
                for step in objects(job, "steps"):
                    if "upload-artifact@" in string(step.get("uses", "")):
                        self.assertIn("!env.ACT", string(step, "if"))

    def test_scheduled_mutations_use_shared_gate_and_never_publish_build_outputs(self) -> None:
        """Test-quality jobs have bounded scope and cannot promote mutated binaries."""
        jobs = obj(workflow("security.yml"), "jobs")
        mutation = obj(jobs, "security-mutation")
        self.assertEqual(mutation["timeout-minutes"], "75")
        self.assertEqual(
            mutation["if"],
            "${{ github.event_name == 'schedule' || github.event_name == 'workflow_dispatch' }}",
        )
        self.assertNotIn("permissions", mutation)
        self.assertNotIn("services", mutation)
        steps = objects(mutation, "steps")
        command = "\n".join(string(step.get("run", "")) for step in steps)
        self.assertIn("cargo fetch --locked", command)
        self.assertIn("build/check-security.sh deep --deep-check mutation", command)
        uploads = [step for step in steps if "upload-artifact@" in string(step.get("uses", ""))]
        self.assertEqual(len(uploads), 1)
        self.assertEqual(
            set(string(uploads[0], "with", "path").splitlines()),
            {
                "${{ runner.temp }}/security-mutation/summary.json",
                "${{ runner.temp }}/security-mutation/mutation/outcome.json",
                "${{ runner.temp }}/security-mutation/mutation/summary.json",
                "${{ runner.temp }}/security-mutation/mutation/inventory.json",
            },
        )
        self.assertNotIn("security-deep", jobs)
        self.assertNotIn("--include-vendor", command)

    def test_codeql_success_requires_original_report_policy_enforcement(self) -> None:
        """An analyzer success or remote dismissal cannot substitute for the local verdict."""
        for name, job in [
            ("source-analysis", obj(workflow("codeql.yml"), "jobs", "source-analysis"))
        ]:
            steps = objects(job, "steps")
            analyze = next(
                step for step in steps if "codeql-action/analyze@" in string(step.get("uses", ""))
            )
            gate = next(
                step
                for step in steps
                if "build/security_codeql_triage.py sarif" in string(step.get("run", ""))
            )
            self.assertLess(steps.index(analyze), steps.index(gate))
            self.assertEqual(at(analyze, "with", "output"), "${{ runner.temp }}/codeql-sarif")
            self.assertEqual(
                gate["if"], "${{ !env.ACT && matrix.language != 'rust' && inputs.security_gate }}"
            )
            self.assertNotIn("continue-on-error", gate)
            command = string(gate, "run")
            self.assertIn("${#reports[@]} != 1", command)
            self.assertIn('--revision "$GITHUB_SHA"', command)
            if name == "native-analysis":
                self.assertIn('--source-cache "$RUNNER_TEMP/vendor-cache"', command)
            uploads = [step for step in steps if "upload-artifact@" in string(step.get("uses", ""))]
            self.assertEqual(len(uploads), 1)
            self.assertEqual(
                uploads[0]["if"], "${{ always() && !env.ACT && inputs.security_gate }}"
            )
            self.assertEqual(
                set(string(uploads[0], "with", "path").splitlines()),
                {
                    "${{ runner.temp }}/codeql-policy-evidence/report.json",
                    "${{ runner.temp }}/codeql-policy-evidence/failure.json",
                    "${{ runner.temp }}/codeql-local/rust-security-policy/report.json",
                    "${{ runner.temp }}/codeql-local/summary.json",
                    "${{ runner.temp }}/codeql-local/rust-security.sarif",
                    "${{ runner.temp }}/codeql-local/*-codeql-analyze-rust-security.stderr",
                },
            )

    def test_rust_uses_exact_database_cache_and_current_upload_categories(self) -> None:
        """Rust alone leaves the source matrix's action analyzer for the shared pinned runner."""
        steps = objects(workflow("codeql.yml"), "jobs", "source-analysis", "steps")
        restore = next(step for step in steps if step.get("id") == "rust-database")
        self.assertEqual(restore["if"], "matrix.language == 'rust'")
        self.assertNotIn("restore-keys", obj(restore, "with"))
        validator = next(step for step in steps if step.get("id") == "rust-reports")
        self.assertEqual(
            validator["if"], "${{ always() && !env.ACT && matrix.language == 'rust' }}"
        )
        self.assertIn(
            "upload_ready(output, os.environ['GITHUB_SHA'], category, 'rust')",
            string(validator, "run"),
        )
        uploads = [
            step for step in steps if "codeql-action/upload-sarif@" in string(step.get("uses", ""))
        ]
        self.assertEqual(
            {string(step, "with", "category") for step in uploads},
            {"/language:rust/security", "/language:rust/quality-advisory"},
        )
        for upload in uploads:
            self.assertIn("always() && !env.ACT && matrix.language == 'rust'", string(upload, "if"))
            self.assertEqual(at(upload, "with", "wait-for-processing"), "true")
            self.assertNotIn("continue-on-error", upload)

    def test_fedora_package_layers_require_current_read_only_policy_inputs(self) -> None:
        """A restored layer cannot bypass changed reviews in either Fedora install stage."""
        recipe = (ROOT / "Dockerfile").read_text().replace("\\\n", " ")
        stages = re.split(r"^FROM .+ AS (\S+)\n", recipe, flags=re.MULTILINE)
        expected = {
            # These are read-only image build mounts, not temporary host files.
            "/image-exceptions.json": "/tmp/simplestchat-image-exceptions.json",  # noqa: S108
            "security/image-policy.json": "/tmp/simplestchat-image-policy.json",  # noqa: S108
        }
        checked: set[str] = set()
        for stage, body in zip(stages[1::2], stages[2::2], strict=True):
            installs = [
                line
                for line in body.splitlines()
                if line.startswith("RUN ") and "dnf upgrade" in line
            ]
            if not installs:
                continue
            self.assertEqual(len(installs), 1)
            command = installs[0]
            tokens = shlex.split(command)
            mounts = [token.removeprefix("--mount=") for token in tokens[1:3]]
            self.assertEqual(len(mounts), 2)
            for mount, (source, target) in zip(mounts, expected.items(), strict=True):
                origin: set[str] = (
                    {"from=image-review-inputs"} if source == "/image-exceptions.json" else set()
                )
                self.assertEqual(
                    set(mount.split(",")),
                    {"type=bind", f"source={source}", f"target={target}", "readonly"} | origin,
                )
                self.assertLess(command.index(f"test -s {target}"), command.index("dnf upgrade"))
                self.assertNotRegex(body, rf"(?m)^COPY .*{re.escape(source)}")
            self.assertIn('test -n "${FEDORA_REFRESH_EPOCH}"', command)
            self.assertIn("dnf upgrade -y --refresh", command)
            self.assertIn("dnf install -y", command)
            checked.add(stage)
        self.assertEqual(checked, {"builder", "runtime-base"})

    def project_image_reviews(self, records: list[JsonObject]) -> bytes:
        """Execute the exact Dockerfile projection with local Node and private fixture files."""
        recipe = (ROOT / "Dockerfile").read_text()
        source = recipe.split("<<'IMAGE_REVIEWS'\n", 1)[1].split("\nIMAGE_REVIEWS", 1)[0]
        self.assertFalse((ROOT / "results").is_symlink())
        (ROOT / "results").mkdir(mode=0o700, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="image-review-projection-", dir=ROOT / "results"
        ) as temporary:
            input_path, output = (
                Path(temporary) / "reviews.json",
                Path(temporary) / "projected.json",
            )
            _ = input_path.write_text(json.dumps({"schemaVersion": 1, "exceptions": records}))
            _ = subprocess.run(  # noqa: S603 -- Exact maintained projection; fixture paths only.
                ["node", "--input-type=module", "-", str(input_path), str(output)],  # noqa: S607 -- Node is a required CI tool.
                input=source,
                text=True,
                check=True,
                capture_output=True,
                timeout=10,
            )
            return output.read_bytes()

    def test_image_review_projection_ignores_unrelated_reviews_and_preserves_complete_records(
        self,
    ) -> None:
        """Only the image audit's three review families affect Fedora package refreshes."""
        records = objects(yaml_value((ROOT / "security/exceptions.json").read_text()), "exceptions")
        projected = self.project_image_reviews(records)
        selected = objects(yaml_value(projected.decode()), "exceptions")
        expected = [
            row for row in records if row["scanner"] in {"grype", "image-license", "gitleaks"}
        ]
        self.assertCountEqual(selected, expected)
        changed = [dict(row) for row in records]
        for row in changed:
            if row["scanner"] == "zizmor":
                row["scope"] = string(row, "scope") + "-moved"
        self.assertEqual(self.project_image_reviews(changed), projected)
        reordered = [dict(reversed(list(row.items()))) for row in reversed(records)]
        self.assertEqual(self.project_image_reviews(reordered), projected)
        for scanner in ("grype", "image-license", "gitleaks"):
            modified = [{**records[0], "scanner": scanner}]
            original = self.project_image_reviews(modified)
            row = modified[0]
            row["rationale"] = string(row, "rationale") + " Updated review."
            with self.subTest(scanner=scanner):
                self.assertNotEqual(self.project_image_reviews(modified), original)

    def test_image_cache_keys_include_both_current_policy_inputs(self) -> None:
        """Saving under a policy-specific key complements the Dockerfile layer dependency."""
        steps = objects(obj(workflow("ci.yml"), "jobs", "deployment"), "steps")
        steps.extend(objects(obj(workflow("load-generator-image.yml"), "jobs", "image"), "steps"))
        caches = [
            obj(step, "with")
            for step in steps
            if string(step.get("uses", "")).startswith("actions/cache")
            and not string(step.get("uses", "")).startswith("actions/cache/save@")
            and step.get("id") != "image-database"
        ]
        self.assertEqual(len(caches), 2)
        for config in caches:
            key = string(config, "key")
            for source in ("security/exceptions.json", "security/image-policy.json"):
                self.assertIn(f"'{source}'", key)
            # A broad fallback is safe only with the checked Dockerfile mount
            # dependency and the existing trust/architecture namespace guard.
            self.assertTrue(string(config, "restore-keys").startswith("buildx-v3-"))

    def test_image_cache_updates_application_layers_without_reusing_a_scan_verdict(self) -> None:
        """Source changes publish new immutable layers while every image check stays required."""
        steps = objects(obj(workflow("ci.yml"), "jobs", "deployment"), "steps")
        restore = next(step for step in steps if step.get("id") == "production-cache")
        key = string(restore, "with", "key")
        for source in ("src/**", "migrations/**", "web/src/**", ".dockerignore"):
            self.assertIn(f"'{source}'", key)
        scan = next(
            step for step in steps if step.get("name") == "Scan the same exported production image"
        )
        self.assertNotIn("if", scan)
        self.assertIn(
            '--image-database-cache "${RUNNER_TEMP}/image-database-cache"', string(scan, "run")
        )

    def test_image_build_exports_layers_only_when_the_primary_cache_is_missing(self) -> None:
        """Run the actual build wrapper through cache hits, misses and a partial failed export."""
        (ROOT / "results").mkdir(exist_ok=True)
        steps = objects(workflow("ci.yml"), "jobs", "deployment", "steps")
        build = next(step for step in steps if step.get("name") == "Build production container")
        self.assertEqual(
            at(build, "env", "CACHE_PRIMARY_HIT"),
            "${{ steps.production-cache.outputs.cache-hit }}",
        )
        save = next(
            step for step in steps if step.get("name") == "Save completed image layer cache"
        )
        self.assertEqual(
            save["if"], "${{ success() && steps.production-cache.outputs.cache-hit != 'true' }}"
        )
        stubs = r"""
git() {
  case "$1" in
    rev-parse) printf '%s\n' "$GITHUB_SHA";;
    status) return 0;;
    *) return 1;;
  esac
}
timeout() {
  [[ "$1 $2 $3" == '--signal=TERM --kill-after=2m 50m' ]]
  shift 3
  "$@"
}
docker() {
  if [[ "$1 $2" != 'buildx build' ]]; then
    [[ "$1 $2" == 'buildx create' || "$1 $2" == 'buildx rm' ]]
    return
  fi
  printf '%s\n' "$@" > "$TEST_BUILD_ARGS"
  shift 2
  while (( $# )); do
    case "$1" in
      --cache-to)
        destination="${2#type=local,dest=}"
        destination="${destination%,mode=max}"
        mkdir -p "$destination"
        printf 'new layers\n' > "$destination/index.json"
        shift 2;;
      --iidfile) printf '%s\n' "$TEST_IMAGE_ID" > "$2"; shift 2;;
      *) shift;;
    esac
  done
  return "$TEST_BUILD_STATUS"
}
"""
        for hit, restored, status in (
            ("true", True, 0),
            ("false", True, 0),
            ("", False, 0),
            ("false", True, 23),
        ):
            with (
                self.subTest(hit=hit, restored=restored, status=status),
                tempfile.TemporaryDirectory(prefix="image cache-", dir=ROOT / "results") as tmp,
            ):
                directory = Path(tmp)
                cache = directory / "buildx-cache"
                if restored:
                    cache.mkdir()
                    _ = (cache / "index.json").write_text("original layers\n")
                    _ = (cache / "existing-layer").write_bytes(b"original layer bytes")
                arguments = directory / "arguments"
                output = directory / "environment"
                image = "sha256:" + "b" * 64
                result = subprocess.run(  # noqa: S603 -- Owned fixture of the actual build wrapper.
                    ["/bin/bash", "-c", stubs + string(build, "run")],
                    env={
                        "PATH": "/usr/bin:/bin",
                        "RUNNER_TEMP": tmp,
                        "GITHUB_SHA": "a" * 40,
                        "GITHUB_ENV": str(output),
                        "PRODUCTION_IMAGE": "fixture:latest",
                        "CACHE_PRIMARY_HIT": hit,
                        "TEST_BUILD_ARGS": str(arguments),
                        "TEST_BUILD_STATUS": str(status),
                        "TEST_IMAGE_ID": image,
                    },
                    capture_output=True,
                    text=True,
                    check=False,
                    timeout=5,
                )
                self.assertEqual(result.returncode, status, result.stderr)
                tokens = arguments.read_text().splitlines()
                self.assertEqual("--cache-from" in tokens, restored)
                if restored:
                    self.assertEqual(
                        tokens[tokens.index("--cache-from") + 1], f"type=local,src={cache}"
                    )
                self.assertEqual("--cache-to" in tokens, hit != "true")
                self.assertEqual(
                    (cache / "index.json").read_text(),
                    "original layers\n" if hit == "true" or status else "new layers\n",
                )
                self.assertEqual((cache / "existing-layer").exists(), hit == "true" or status != 0)
                self.assertEqual((directory / "buildx-cache-next").exists(), status != 0)
                self.assertEqual(
                    output.read_text() if output.exists() else "",
                    "" if status else f"PRODUCTION_IMAGE={image}\n",
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
                if string(step, "uses").startswith("actions/cache/save@"):
                    # A split save must inherit the exact validated restore key;
                    # resolve it before checking the trust/architecture inputs.
                    match = re.fullmatch(
                        r"\$\{\{ steps\.([a-zA-Z0-9_-]+)\.outputs\.cache-primary-key \}\}",
                        string(config, "key"),
                    )
                    self.assertIsNotNone(match)
                    if match is None:
                        continue
                    restore = next(step for step in steps if step.get("id") == match.group(1))
                    self.assertTrue(string(restore, "uses").startswith("actions/cache/restore@"))
                    self.assertEqual(config["path"], at(restore, "with", "path"))
                    config = obj(restore, "with")
                key = string(config.get("prefix-key", config.get("key", "")))
                if key == "${{ steps.image-database-inputs.outputs.key }}":
                    # Database bytes have no CPU architecture. Their helper
                    # tests bind scanner format, main/PR trust and daily update.
                    self.assertEqual(
                        config["restore-keys"], "${{ steps.image-database-inputs.outputs.prefix }}"
                    )
                    fingerprint = next(
                        step for step in steps if step.get("id") == "image-database-inputs"
                    )
                    self.assertIn(
                        "python3 build/security_image_cache.py", string(fingerprint, "run")
                    )
                    continue
                if string(step, "uses").startswith("Swatinem/rust-cache@"):
                    self.assertTrue(
                        key.startswith("v3-rust-"), "Old archives included CodeQL tools"
                    )
                    self.assertNotIn("cache-targets", config, "Keep Rust target artifact caching")
                self.assertIn("github.event_name != 'pull_request'", key)
                self.assertIn("'main' || 'untrusted'", key)
                self.assertIn("runner.arch", key)
                if "restore-keys" in config:
                    self.assertIn("'main' || 'untrusted'", string(config, "restore-keys"))


if __name__ == "__main__":
    _ = unittest.main()
