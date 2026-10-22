"""Offline policy checks for disposable-host release integration in CI."""

import unittest
from functools import cached_property
from pathlib import Path

from test_support import at, obj, objects, string, yaml_value

# isort: split
from release_json import JsonObject

ROOT = Path(__file__).resolve().parents[3]
WORKFLOW = ROOT / ".github/workflows/ci.yml"
COMPOSE_TOOL = "Install checksum-pinned production Compose"
PREPARE = "Install isolated release-container checks"
PREPARE_START = "Start the isolated release-container controller build"
INTEGRATION = "App-only release and rollback on a disposable host"
RETENTION = "Preserve sanitized release-container report"
LOCAL_DIAGNOSTICS = "Preserve private local release-container diagnostics"
EXPORT = "Export the tested immutable production image"
RELEASE = "Retain the verified production release"
RUN_COMMAND = (
    "python3 -B build/ci_release_checks.py \\\n"
    + '  --controller "${RUNNER_TEMP}/release-container-controller/bin/python" \\\n'
    + '  --node "$(command -v node)" \\\n'
    + "  --chromium \"$(node -e 'process.stdout.write(require("
    + '"./web/e2e/node_modules/playwright").chromium.executablePath())\')" \\\n'
    + '  --image "${PRODUCTION_IMAGE}" \\\n'
    + '  --fixture-output "${RUNNER_TEMP}/release-container" \\\n'
    + '  --output "${RUNNER_TEMP}/release-checks"\n'
)


class ReleaseContainerCiTests(unittest.TestCase):
    """Verify the release container ci contract offline."""

    @cached_property
    def workflow(self) -> JsonObject:
        """Read one workflow fixture without YAML 1.1 boolean-key coercion."""
        # BaseLoader preserves GitHub's YAML 1.2 `on` key and compares scalars
        # without PyYAML's YAML 1.1 implicit boolean conversions.
        return obj(yaml_value(WORKFLOW.read_text(), scalars_as_strings=True))

    @cached_property
    def job(self) -> JsonObject:
        """Require the production deployment job to be a mapping."""
        return obj(self.workflow, "jobs", "deployment")

    @cached_property
    def steps(self) -> list[JsonObject]:
        """Read the ordered job steps with checked object boundaries."""
        return objects(self.job, "steps")

    def step(self, name: str) -> JsonObject:
        """Find exactly one named step, failing on missing or ambiguous wiring."""
        matches = [step for step in self.steps if step.get("name") == name]
        self.assertEqual(len(matches), 1, f"Expected exactly one {name!r} step")
        return matches[0]

    def test_fresh_host_and_job_deadline_include_the_existing_build(self) -> None:
        """Verify fresh host and job deadline include the existing build."""
        self.assertEqual(self.job["runs-on"], "ubuntu-24.04")
        self.assertEqual(self.job["timeout-minutes"], "95")
        self.assertNotIn("container", self.job, "The fixture uses the runner's own Docker daemon")
        self.assertNotIn("services", self.job)
        self.assertEqual(at(self.job, "env", "PRODUCTION_IMAGE"), "simplestchat-ci:production")
        self.assertNotIn("continue-on-error", self.job)

    def test_existing_image_is_built_and_smoked_once_before_release_integration(self) -> None:
        """Verify existing image is built and smoked once before release integration."""
        expected = [
            COMPOSE_TOOL,
            "Validate Compose rendering",
            "Build production container",
            "Verify production image contents and user",
            "Production startup, migration and database-backed API smoke",
            EXPORT,
            PREPARE,
            INTEGRATION,
            RELEASE,
            RETENTION,
        ]
        indexes = [self.steps.index(self.step(name)) for name in expected]
        self.assertEqual(indexes, sorted(indexes))
        build = string(self.step("Build production container"), "run")
        self.assertEqual(build, "cargo run --locked -q -p xtask -- ci production-image build")
        build_steps = [
            step
            for step in self.steps
            if "ci production-image build" in string(step.get("run", ""))
        ]
        self.assertEqual(build_steps, [self.step("Build production container")])

    def test_one_revision_labeled_immutable_image_feeds_all_smokes_and_export(self) -> None:
        """Verify one revision labeled immutable image feeds all smokes and export."""
        build = self.step("Build production container")
        self.assertEqual(build["shell"], "bash")
        command = string(build, "run")
        self.assertEqual(command, "cargo run --locked -q -p xtask -- ci production-image build")
        for step in self.steps[self.steps.index(build) + 1 :]:
            self.assertNotIn(
                "PRODUCTION_IMAGE",
                obj(step.get("env", {})),
                "Do not replace the checked immutable ID",
            )
        verify = string(self.step("Verify production image contents and user"), "run")
        self.assertEqual(verify, "cargo run --locked -q -p xtask -- ci production-image verify")
        smoke = self.step("Production startup, migration and database-backed API smoke")
        self.assertEqual(smoke["run"], "build/test-container.sh")
        self.assertIn(
            "${PRODUCTION_IMAGE:-simplestchat-ci:production}",
            (ROOT / "build/test-container.sh").read_text(),
        )
        self.assertIn('--image "${PRODUCTION_IMAGE}"', string(self.step(INTEGRATION), "run"))

    def test_image_policy_has_pinned_rust_on_hosted_and_disposable_ci(self) -> None:
        """The compiled policy runs in both contexts without changing action scope."""
        installer = self.steps[1]
        self.assertEqual(installer["name"], "Install the pinned Rust toolchain for CI image policy")
        self.assertNotIn("if", installer)
        self.assertEqual(
            installer["uses"], "dtolnay/rust-toolchain@89b12181fb390509a0842a86cc55eeb8eb928c1d"
        )
        self.assertEqual(at(installer, "with", "toolchain"), "1.99.0")
        image_database = objects(self.workflow, "jobs", "image-database", "steps")
        database = image_database[1]
        self.assertEqual(
            database["name"], "Install the pinned Rust toolchain for CI database cache policy"
        )
        self.assertNotIn("if", database)
        self.assertEqual(database["uses"], installer["uses"])
        self.assertEqual(database["with"], installer["with"])

    def test_push_export_never_rebuilds_and_publication_waits_for_required_checks(self) -> None:
        """Verify push export never rebuilds and publication waits for required checks."""
        self.assertEqual(at(self.workflow, "on", "push", "branches"), ["main"])
        export = self.step(EXPORT)
        self.assertNotIn("if", export, "PR image policy must inspect the canonical export too")
        self.assertEqual(export["timeout-minutes"], "6")
        self.assertEqual(
            string(export, "run"),
            (
                'python3 build/build-release.py --image-id "${PRODUCTION_IMAGE}" \\\n'
                '  --timeout-seconds 300 --output "${RUNNER_TEMP}/simplestchat-release"\n'
            ),
        )
        self.assertNotIn("continue-on-error", export)
        self.assertNotIn(
            "sudo",
            string(export, "run"),
            "Export before root fixture state exists; preserve exporter guards",
        )
        release = self.step(RELEASE)
        self.assertEqual(
            release["if"], "${{ success() && github.event_name == 'push' && !env.ACT }}"
        )
        self.assertRegex(string(release, "uses"), r"^actions/upload-artifact@[a-f0-9]{40}$")
        self.assertEqual(
            release["with"],
            {
                "name": "simplestchat-unsigned-${{ github.sha }}",
                "path": "${{ runner.temp }}/simplestchat-release",
                "if-no-files-found": "error",
                "compression-level": "0",
                "retention-days": "14",
            },
        )
        self.assertLess(self.steps.index(export), self.steps.index(self.step(INTEGRATION)))
        self.assertLess(self.steps.index(self.step(INTEGRATION)), self.steps.index(release))
        image_check = self.step("Scan the same exported production image")
        self.assertNotIn("if", image_check)
        self.assertNotIn("continue-on-error", image_check)
        self.assertLess(self.steps.index(self.step(INTEGRATION)), self.steps.index(image_check))
        self.assertLess(self.steps.index(image_check), self.steps.index(release))
        self.assertIn(
            'build/check-security.sh image "${PRODUCTION_IMAGE}"', string(image_check, "run")
        )
        self.assertIn(
            '--artifact-dir "${RUNNER_TEMP}/simplestchat-release"', string(image_check, "run")
        )
        self.assertIn(
            '--image-tools "${RUNNER_TEMP}/release-checks/tools"', string(image_check, "run")
        )
        self.assertNotIn("continue-on-error", release)

    def test_compose_is_verified_before_install_and_matches_both_execution_users(self) -> None:
        """Verify compose is verified before install and matches both execution users."""
        step = self.step(COMPOSE_TOOL)
        self.assertEqual(step["timeout-minutes"], "3")
        self.assertEqual(step["shell"], "bash")
        self.assertNotIn("continue-on-error", step)
        self.assertNotIn("if", step)
        self.assertEqual(
            string(step, "run"),
            "cargo run --locked -q -p xtask -- ci install-compose",
        )
        self.assertLess(
            self.steps.index(step), self.steps.index(self.step("Validate Compose rendering"))
        )
        self.assertLess(
            self.steps.index(step), self.steps.index(self.step("Build production container"))
        )
        # Pin selection, verified bytes, literal privilege/TLS argv, both version
        # checks and failure/cancellation ownership use actual Rust/fixture tests.

    def test_guest_compose_contract_runs_with_required_pinned_renderer(self) -> None:
        """The real JSON contract regression cannot silently skip in production-image CI."""
        step = self.step("Verify the guest checker against pinned Compose JSON")
        self.assertNotIn("if", step)
        self.assertNotIn("continue-on-error", step)
        self.assertEqual(step["timeout-minutes"], "2")
        self.assertEqual(string(step, "env", "VM_COMPOSE_REQUIRED"), "1")
        self.assertIn(
            '"${RUNNER_TEMP}/release-container-controller/bin/python" -B -m unittest discover',
            string(step, "run"),
        )
        self.assertIn(
            "-s ops/ansible/tests -p test_security_vm_guest.py -k ComposeSerializationTests -v",
            string(step, "run"),
        )
        position = self.steps.index(step)
        self.assertLess(self.steps.index(self.step(COMPOSE_TOOL)), position)
        self.assertLess(
            self.steps.index(self.step("Install isolated release-container checks")), position
        )
        self.assertLess(position, self.steps.index(self.step(INTEGRATION)))

    def test_dependencies_use_existing_pinned_controller_requirements_in_an_isolated_venv(
        self,
    ) -> None:
        """Verify dependencies use existing pinned controller requirements in an isolated venv."""
        start = self.step(PREPARE_START)
        command = string(start, "run")
        for line in (
            "set -euo pipefail\n",
            "sudo apt-get update\n",
            "sudo apt-get install -y python3-venv openssl curl ca-certificates util-linux\n",
            'python3 -m venv "${RUNNER_TEMP}/release-container-controller"\n',
            '"${RUNNER_TEMP}/release-container-controller/bin/pip" '
            + "install -r ops/ansible/requirements.txt\n",
            "set +e\n",
            'echo "$?" > "${RUNNER_TEMP}/release-container-controller.status"\n',
        ):
            self.assertIn(line, command)
        self.assertTrue(command.rstrip().endswith("< /dev/null > /dev/null 2>&1 &"))
        # The controller build overlaps the image build instead of following it.
        self.assertLess(
            self.steps.index(start), self.steps.index(self.step("Build production container"))
        )
        prepare = self.step(PREPARE)
        self.assertEqual(prepare["timeout-minutes"], "5")
        self.assertEqual(
            prepare["run"],
            (
                'until [[ -f "${RUNNER_TEMP}/release-container-controller.status" ]]; '
                + "do sleep 1; done\n"
                + 'cat "${RUNNER_TEMP}/release-container-controller.log"\n'
                + 'test "$(<"${RUNNER_TEMP}/release-container-controller.status")" = 0\n'
                + 'test -x "${RUNNER_TEMP}/release-container-controller/bin/python"\n'
            ),
        )
        for step in (start, prepare):
            self.assertNotIn("continue-on-error", step)
            self.assertNotIn("if", step)

    def test_root_disposable_opt_in_is_explicit_bounded_and_preserves_failure(self) -> None:
        """Verify root disposable opt in is explicit bounded and preserves failure."""
        integration = self.step(INTEGRATION)
        self.assertEqual(integration["run"], RUN_COMMAND)
        self.assertEqual(integration["timeout-minutes"], "11")
        self.assertNotIn("continue-on-error", integration)
        self.assertNotIn("if", integration, "Do not silently skip a required release check")
        self.assertNotIn(
            "env", integration, "No deployment secrets or helper overrides enter the fixture"
        )

    def test_pinned_rollover_browser_runs_after_image_build_without_an_extra_app_build(
        self,
    ) -> None:
        """Cache only browser download bytes; pinned installation remains unconditional."""
        setup = [
            step
            for step in self.steps
            if string(step.get("uses", "")).startswith("actions/setup-node@")
        ]
        self.assertEqual(len(setup), 1)
        self.assertEqual(
            setup[0]["uses"], "actions/setup-node@820762786026740c76f36085b0efc47a31fe5020"
        )
        self.assertEqual(
            setup[0]["with"],
            {
                "node-version": "26.10.0",
                "cache": "npm",
                "cache-dependency-path": "web/e2e/package-lock.json",
            },
        )
        npm = self.step("Install checksum-pinned npm")
        self.assertIn(
            'node build/install-npm.mjs "$RUNNER_TEMP/simplestchat-npm-12.2.0"',
            string(npm, "run"),
        )
        restore = self.step("Restore pinned rollover Chromium download bytes")
        tooling = self.step("Install pinned rollover browser tooling")
        save = self.step("Save pinned rollover Chromium download bytes")
        self.assertEqual(restore["id"], "rollover-chromium")
        self.assertEqual(
            restore["uses"], "actions/cache/restore@55cc8345863c7cc4c66a329aec7e433d2d1c52a9"
        )
        self.assertEqual(
            restore["with"],
            {
                "path": "${{ runner.temp }}/rollover-playwright",
                "key": "rollover-chromium-v1-${{ runner.os }}-${{ runner.arch }}-"
                + "${{ hashFiles('web/e2e/package-lock.json') }}",
            },
        )
        self.assertEqual(tooling["timeout-minutes"], "5")
        self.assertEqual(
            tooling["env"], {"PLAYWRIGHT_BROWSERS_PATH": "${{ runner.temp }}/rollover-playwright"}
        )
        command = string(tooling, "run")
        self.assertIn("npm --prefix web/e2e ci --ignore-scripts\n", command)
        self.assertIn(
            "npm --prefix web/e2e exec -- playwright install --with-deps chromium\n", command
        )
        self.assertIn(
            'echo "PLAYWRIGHT_BROWSERS_PATH=$RUNNER_TEMP/rollover-playwright" >> "$GITHUB_ENV"',
            command,
        )
        self.assertEqual(save["if"], "steps.rollover-chromium.outputs.cache-hit != 'true'")
        self.assertEqual(
            save["uses"], "actions/cache/save@55cc8345863c7cc4c66a329aec7e433d2d1c52a9"
        )
        self.assertEqual(
            save["with"],
            {
                "path": "${{ runner.temp }}/rollover-playwright",
                "key": "${{ steps.rollover-chromium.outputs.cache-primary-key }}",
            },
        )
        ordered = [
            self.step("Build production container"),
            setup[0],
            npm,
            restore,
            tooling,
            save,
            self.step(INTEGRATION),
        ]
        positions = [self.steps.index(step) for step in ordered]
        self.assertEqual(positions, sorted(positions))
        for step in (setup[0], npm, tooling):
            self.assertNotIn("if", step)
            self.assertNotIn("continue-on-error", step)
        commands = "\n".join(string(step.get("run", "")) for step in self.steps)
        self.assertNotIn("npm --prefix web run build", commands)

    def test_only_sanitized_report_is_retained_even_on_failure(self) -> None:
        """Verify only sanitized report is retained even on failure."""
        retention = self.step(RETENTION)
        self.assertRegex(string(retention, "uses"), r"^actions/upload-artifact@[a-f0-9]{40}$")
        self.assertEqual(retention["if"], "${{ !cancelled() && !env.ACT }}")
        self.assertEqual(
            retention["with"],
            {
                "name": "release-container",
                "path": "${{ runner.temp }}/release-container/report.json",
                "if-no-files-found": "warn",
                "retention-days": "7",
            },
        )
        self.assertNotIn("run", retention, "Retention must not rewrite a failed test outcome")
        self.assertNotIn("continue-on-error", retention)
        uploads = [
            step for step in self.steps if "upload-artifact@" in string(step.get("uses", ""))
        ]
        self.assertEqual(
            {string(step, "with", "path") for step in uploads},
            {
                "${{ runner.temp }}/container-smoke",
                "${{ runner.temp }}/release-container/report.json",
                "${{ runner.temp }}/simplestchat-release",
                "\n".join(
                    "${{ runner.temp }}/production-security/image/" + name
                    for name in (
                        "outcome.json",
                        "checks.json",
                        "elf.json",
                        "runtime-proof.json",
                        "vex.openvex.json",
                        "runtime-license-evidence.json",
                        "native.json",
                        "database-status.json",
                        "scanner-result-*.json",
                        "secret-paths.json",
                        "sbom/sbom.syft.json",
                        "spdx/sbom.spdx.json",
                    )
                )
                + "\n",
            },
            "Do not upload private release fixture directories or broad runner paths",
        )

    def test_completed_layers_are_saved_before_later_checks_can_fail(self) -> None:
        """A successful build survives fixture failures without saving partial layers."""
        restore = self.step("Restore image layer cache")
        save = self.step("Save completed image layer cache")
        pin = "55cc8345863c7cc4c66a329aec7e433d2d1c52a9"
        self.assertEqual(restore["uses"], f"actions/cache/restore@{pin}")
        self.assertEqual(restore["id"], "production-cache")
        self.assertEqual(save["uses"], f"actions/cache/save@{pin}")
        self.assertEqual(
            save["if"],
            "${{ success() && steps.production-cache.outputs.cache-hit != 'true' }}",
        )
        self.assertEqual(
            save["with"],
            {
                "path": at(restore, "with", "path"),
                "key": "${{ steps.production-cache.outputs.cache-primary-key }}",
            },
        )
        self.assertEqual(
            self.steps.index(save), self.steps.index(self.step("Build production container")) + 1
        )
        self.assertLess(
            self.steps.index(save),
            self.steps.index(self.step("Verify production image contents and user")),
        )
        self.assertLess(self.steps.index(save), self.steps.index(self.step(INTEGRATION)))
        for step in (restore, save):
            self.assertNotIn("continue-on-error", step)

    def test_local_failed_fixture_uses_the_compiled_private_evidence_policy(self) -> None:
        """Keep diagnostics after failures; Rust fixtures check copied bytes and permissions."""
        retention = self.step(LOCAL_DIAGNOSTICS)
        self.assertEqual(retention["if"], "${{ always() && env.ACT == 'true' }}")
        self.assertEqual(self.steps.index(retention), self.steps.index(self.step(INTEGRATION)) + 1)
        self.assertEqual(
            retention["run"], "cargo run --locked -q -p xtask -- ci copy-evidence release-container"
        )
        self.assertEqual(retention["shell"], "bash")
        self.assertNotIn("env", retention)
        self.assertNotIn("continue-on-error", retention)

    def test_pinned_compiler_precedes_image_policy_and_all_guarded_local_evidence(self) -> None:
        """Image policy always compiles; local-only evidence copies retain their guards."""
        for job_name, copies in (
            ("image-database", {"image-database"}),
            ("deployment", {"release-container", "release-checks"}),
        ):
            steps = objects(obj(self.workflow, "jobs", job_name), "steps")
            bootstrap = steps[1]
            self.assertEqual(
                bootstrap["uses"],
                "dtolnay/rust-toolchain@89b12181fb390509a0842a86cc55eeb8eb928c1d",
            )
            self.assertNotIn("if", bootstrap)
            self.assertEqual(bootstrap["with"], {"toolchain": "1.99.0"})
            self.assertNotIn("env", bootstrap)
            self.assertNotIn("permissions", bootstrap)
            self.assertNotIn("environment", bootstrap)
            self.assertNotIn("continue-on-error", bootstrap)
            commands = {
                string(step, "run").rsplit(" ", 1)[-1]: step
                for step in steps
                if "ci copy-evidence " in string(step.get("run", ""))
            }
            self.assertEqual(set(commands), copies)
            for kind, step in commands.items():
                self.assertEqual(step["if"], "${{ always() && env.ACT == 'true' }}")
                self.assertEqual(
                    step["run"], f"cargo run --locked -q -p xtask -- ci copy-evidence {kind}"
                )
                self.assertNotIn("env", step)
                self.assertNotIn("continue-on-error", step)
                self.assertGreater(steps.index(step), 1)

    def test_read_only_permissions_pinned_actions_and_no_remote_deployment(self) -> None:
        """Verify read only permissions pinned actions and no remote deployment."""
        self.assertEqual(self.workflow["permissions"], {"contents": "read"})
        self.assertNotIn("permissions", self.job)
        self.assertNotIn("environment", self.job)
        for step in self.steps:
            if "uses" in step:
                if string(step, "uses").startswith("dtolnay/rust-toolchain@"):
                    self.assertEqual(
                        step, self.steps[1], "Only the fixed local-evidence bootstrap is allowed"
                    )
                    continue
                # First-party GitHub actions only, pinned by commit: checkout,
                # Node tooling, download/layer caches, and artifact retention.
                self.assertRegex(
                    string(step, "uses"),
                    r"^actions/(?:checkout|setup-node|cache/(?:restore|save)|upload-artifact)@[a-f0-9]{40}$",
                )
            self.assertNotIn("permissions", step)
            self.assertNotIn("environment", step)
        checkout = next(
            step
            for step in self.steps
            if string(step.get("uses", "")).startswith("actions/checkout@")
        )
        self.assertEqual(checkout["with"], {"persist-credentials": "false"})
        commands = "\n".join(string(step.get("run", "")) for step in self.steps)
        self.assertNotRegex(commands, r"\b(?:ssh|scp|ansible-playbook|kubectl|reboot)\b")
        self.assertNotRegex(commands, r"\bdocker\s+(?:push|login)\b")
        self.assertNotRegex(str(self.job), r"\$\{\{\s*secrets\.")


if __name__ == "__main__":
    _ = unittest.main()
