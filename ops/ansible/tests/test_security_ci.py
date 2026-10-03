"""Protect CI trust boundaries, gate completeness and release evidence selection."""

from __future__ import annotations

import re
import shlex
import subprocess
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
        self.assertEqual(set(obj(codeql, "jobs")), {"source-analysis", "native-analysis"})
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
        for name, job in obj(workflow("codeql.yml"), "jobs").items():
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
                self.assertEqual(init["if"], "${{ !env.ACT }}")
                self.assertEqual(analyze["if"], "${{ !env.ACT }}")

    def test_codeql_requires_manual_observed_native_compilation(self) -> None:
        """A source-only or restored compilation cannot satisfy native CodeQL coverage."""
        codeql = workflow("codeql.yml")
        native = obj(codeql, "jobs", "native-analysis")
        self.assertEqual(at(codeql, "jobs", "source-analysis", "runs-on"), "ubuntu-24.04")
        self.assertEqual(native["runs-on"], "ubuntu-24.04")
        steps = objects(native, "steps")
        self.assertEqual(
            string(steps[0], "run"),
            'test "$(uname -s)" = Linux && [[ "$(uname -m)" =~ ^(x86_64|aarch64)$ ]]',
        )
        self.assertNotIn("if", steps[0])
        self.assertNotIn("continue-on-error", steps[0])
        init = next(step for step in steps if step.get("id") == "init")
        self.assertEqual(at(init, "with", "build-mode"), "manual")
        build = next(
            step
            for step in steps
            if step.get("name") == "Compile the actual worker under CodeQL tracing"
        )
        command = string(build, "run")
        self.assertEqual(command, "bash build/codeql-native-build.sh")
        command = (ROOT / "build/codeql-native-build.sh").read_text()
        self.assertIn("libmediasoup-worker", command)
        self.assertIn('mktemp -d "$project_root/target/codeql-worker.XXXXXXXX"', command)
        self.assertIn("build/security_codeql_resources.py", command)
        self.assertNotIn("docker", command)
        self.assertNotIn("env -i", command)
        self.assertLess(steps.index(init), steps.index(build))
        self.assertTrue(
            any("build/security_codeql.py" in string(step.get("run", "")) for step in steps)
        )
        caches = [step for step in steps if "cache@" in string(step.get("uses", ""))]
        self.assertEqual(len(caches), 1)
        self.assertEqual(at(caches[0], "with", "path"), "${{ env.OPENSSL_DIR }}")
        self.assertIn("hashFiles('build/install-openssl.sh')", string(caches[0], "with", "key"))
        self.assertIn("'main' || 'untrusted'", string(caches[0], "with", "key"))
        self.assertIn("runner.os", string(caches[0], "with", "key"))
        self.assertIn("runner.arch", string(caches[0], "with", "key"))
        self.assertNotIn("restore-keys", obj(caches[0], "with"))
        validation = next(
            step for step in steps if step.get("name") == "Validate static OpenSSL prerequisites"
        )
        self.assertLess(steps.index(validation), steps.index(build))
        self.assertIn("pkg-config --exact-version=3.5.9 openssl", string(validation, "run"))

    def test_native_codeql_guard_accepts_supported_targets_and_rejects_other_hosts(self) -> None:
        """Exercise the real guard without starting the compiler or contacting a runner."""
        native = obj(workflow("codeql.yml"), "jobs", "native-analysis")
        command = string(objects(native, "steps")[0], "run")
        script = (
            'uname() { case "$1" in -s) printf "%s\\n" "$TEST_SYSTEM";; '
            + '-m) printf "%s\\n" "$TEST_MACHINE";; *) return 1;; esac; }\n'
            + command
        )
        for system, machine, accepted in (
            ("Linux", "x86_64", True),
            ("Linux", "aarch64", True),
            ("Linux", "riscv64", False),
            ("Darwin", "aarch64", False),
        ):
            with self.subTest(system=system, machine=machine):
                result = subprocess.run(  # noqa: S603 -- Run only the checked local platform guard.
                    ["/bin/bash", "-c", script],
                    env={"TEST_SYSTEM": system, "TEST_MACHINE": machine},
                    capture_output=True,
                    text=True,
                    check=False,
                    timeout=5,
                )
                self.assertEqual(result.returncode == 0, accepted, result.stderr)

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

    def test_native_matrix_keeps_all_modes_with_shared_preparation_and_isolation(self) -> None:
        """Parallelism must preserve each complete sanitizer/corpus suite and its evidence."""
        native = obj(workflow("security.yml"), "jobs", "native-security")
        self.assertEqual(strings(native, "strategy", "matrix", "mode"), ["asan", "ubsan", "replay"])
        self.assertEqual(at(native, "strategy", "fail-fast"), "false")
        self.assertEqual(at(native, "strategy", "max-parallel"), "3")
        steps = objects(native, "steps")
        command = "\n".join(string(step.get("run", "")) for step in steps)
        for required in (
            "build/ci-local-docker.sh",
            "build/security_vendor.py verify",
            "build/native_security.py verify",
            "build/native_security.py prepare",
            'build/native_security.py run --image "$image_id" --mode "$NATIVE_MODE"',
        ):
            self.assertIn(required, command)
        recorder = next(
            step for step in steps if "ci-local-evidence.py record" in string(step.get("run", ""))
        )
        self.assertEqual(recorder["if"], "${{ env.ACT }}")
        self.assertEqual(at(recorder, "env", "NATIVE_MODE"), "${{ matrix.mode }}")
        self.assertIn('--id "native-$NATIVE_MODE"', string(recorder, "run"))
        self.assertFalse(any("continue-on-error" in step for step in steps))

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
        self.assertIn("security_dependency_licenses.check(context, snapshot, base)", source)
        self.assertIn("security_actions.check(context, snapshot, base)", source)
        self.assertIn('vulnerabilities["total"] == 0', source)
        self.assertIn('"pip_audit"', source)

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
                self.assertEqual(local["if"], "${{ env.ACT }}")
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
                hosted_pin = next(
                    step
                    for step in steps
                    if "build/security_codeql_tools.py" in string(step.get("run", ""))
                )
                self.assertEqual(hosted_pin["if"], "${{ !env.ACT }}")
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
        native = "\n".join(
            string(step.get("run", "")) for step in objects(jobs, "security-deep", "steps")
        )
        self.assertIn("build/check-security.sh deep --deep-check native", native)

    def test_codeql_success_requires_original_report_policy_enforcement(self) -> None:
        """An analyzer success or remote dismissal cannot substitute for the local verdict."""
        for name, job in obj(workflow("codeql.yml"), "jobs").items():
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
            self.assertEqual(gate["if"], "${{ !env.ACT && inputs.security_gate }}")
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
                },
            )

    def test_fedora_package_layers_require_current_read_only_policy_inputs(self) -> None:
        """A restored layer cannot bypass changed reviews in either Fedora install stage."""
        recipe = (ROOT / "Dockerfile").read_text().replace("\\\n", " ")
        stages = re.split(r"^FROM .+ AS (\S+)\n", recipe, flags=re.MULTILINE)
        expected = {
            # These are read-only image build mounts, not temporary host files.
            "security/exceptions.json": "/tmp/simplestchat-image-exceptions.json",  # noqa: S108
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
                self.assertEqual(
                    set(mount.split(",")),
                    {"type=bind", f"source={source}", f"target={target}", "readonly"},
                )
                self.assertLess(command.index(f"test -s {target}"), command.index("dnf upgrade"))
                self.assertNotRegex(body, rf"(?m)^COPY .*{re.escape(source)}")
            self.assertIn('test -n "${FEDORA_REFRESH_EPOCH}"', command)
            self.assertIn("dnf upgrade -y --refresh", command)
            self.assertIn("dnf install -y", command)
            checked.add(stage)
        self.assertEqual(checked, {"builder", "runtime-base"})

    def test_image_cache_keys_include_both_current_policy_inputs(self) -> None:
        """Saving under a policy-specific key complements the Dockerfile layer dependency."""
        steps = objects(obj(workflow("ci.yml"), "jobs", "deployment"), "steps")
        steps.extend(objects(obj(workflow("load-generator-image.yml"), "jobs", "image"), "steps"))
        caches = [
            obj(step, "with")
            for step in steps
            if string(step.get("uses", "")).startswith("actions/cache")
            and not string(step.get("uses", "")).startswith("actions/cache/save@")
        ]
        self.assertEqual(len(caches), 2)
        for config in caches:
            key = string(config, "key")
            for source in ("security/exceptions.json", "security/image-policy.json"):
                self.assertIn(f"'{source}'", key)
            # A broad fallback is safe only with the checked Dockerfile mount
            # dependency and the existing trust/architecture namespace guard.
            self.assertTrue(string(config, "restore-keys").startswith("buildx-v2-"))

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
                self.assertIn("github.event_name != 'pull_request'", key)
                self.assertIn("'main' || 'untrusted'", key)
                self.assertIn("runner.arch", key)
                if "restore-keys" in config:
                    self.assertIn("'main' || 'untrusted'", string(config, "restore-keys"))


if __name__ == "__main__":
    _ = unittest.main()
