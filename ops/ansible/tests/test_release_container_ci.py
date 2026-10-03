"""Offline policy checks for disposable-host release integration in CI."""

import re
import subprocess
import tempfile
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
EXPORT = "Export the tested immutable production image"
RELEASE = "Retain the verified production release"
RUN_COMMAND = (
    'sudo "${RUNNER_TEMP}/release-container-controller/bin/python" -B \\\n'
    "  build/test-release-container.py --disposable-host \\\n"
    '  --image "${PRODUCTION_IMAGE}" --output "${RUNNER_TEMP}/release-container"\n'
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
        self.assertIn('--tag "${PRODUCTION_IMAGE}" .', build)
        build_steps = [
            step
            for step in self.steps
            if re.search(r"\bdocker\s+(?:build|buildx\s+build)\b", string(step.get("run", "")))
        ]
        self.assertEqual(build_steps, [self.step("Build production container")])

    def test_one_revision_labeled_immutable_image_feeds_all_smokes_and_export(self) -> None:
        """Verify one revision labeled immutable image feeds all smokes and export."""
        build = self.step("Build production container")
        self.assertEqual(build["shell"], "bash")
        command = string(build, "run")
        self.assertTrue(command.startswith("set -euo pipefail\n"))
        self.assertIn('test "$(git rev-parse HEAD)" = "${GITHUB_SHA}"', command)
        self.assertIn('test -z "$(git status --porcelain=v1 --untracked-files=all)"', command)
        self.assertIn('--label "org.opencontainers.image.revision=${GITHUB_SHA}"', command)
        self.assertIn('--iidfile "${RUNNER_TEMP}/production.iid"', command)
        self.assertIn('production_id="$(<"${RUNNER_TEMP}/production.iid")"', command)
        self.assertIn('[[ "${production_id}" =~ ^sha256:[a-f0-9]{64}$ ]]', command)
        self.assertIn(
            'printf \'PRODUCTION_IMAGE=%s\\n\' "${production_id}" >> "${GITHUB_ENV}"', command
        )
        self.assertLess(command.index("git status"), command.index("docker build"))
        self.assertLess(command.index("docker build"), command.index('"${GITHUB_ENV}"'))
        for step in self.steps[self.steps.index(build) + 1 :]:
            self.assertNotIn(
                "PRODUCTION_IMAGE",
                obj(step.get("env", {})),
                "Do not replace the checked immutable ID",
            )
        verify = string(self.step("Verify production image contents and user"), "run")
        self.assertIn('"${PRODUCTION_IMAGE}" -eu -c', verify)
        smoke = self.step("Production startup, migration and database-backed API smoke")
        self.assertEqual(smoke["run"], "build/test-container.sh")
        self.assertIn(
            "${PRODUCTION_IMAGE:-simplestchat-ci:production}",
            (ROOT / "build/test-container.sh").read_text(),
        )
        self.assertIn('--image "${PRODUCTION_IMAGE}"', string(self.step(INTEGRATION), "run"))

    def run_builder_selection(
        self,
        machine: str,
        *,
        act: str = "true",
        overrides: dict[str, str] | None = None,
    ) -> tuple[subprocess.CompletedProcess[str], list[str]]:
        """Execute actual workflow routing with Docker calls replaced by inert records."""
        command = string(self.step("Build production container"), "run")
        _, marker, selection = command.partition("driver_options=()\n")
        self.assertTrue(marker)
        selection, marker, _ = selection.partition('cache_dir="')
        self.assertTrue(marker)
        mocks = r"""
uname() { printf '%s\n' "$TEST_MACHINE"; }
timeout() { shift 3; "$@"; }
docker() {
  printf '%s ' "$@" >> "$RUNNER_TEMP/docker-calls"
  printf '\n' >> "$RUNNER_TEMP/docker-calls"
  case "$1 $2" in
    'buildx build')
      [[ "$TEST_BUILD_STATUS" == 0 ]] || return "$TEST_BUILD_STATUS"
      while (($#)); do
        if [[ "$1" == --iidfile ]]; then
          shift
          printf '%s\n' "$TEST_IMAGE_ID" > "$1"
          break
        fi
        shift
      done
      ;;
    'image inspect') printf '%s\n' "$TEST_IMAGE_ARCH" ;;
  esac
}
"""
        (ROOT / "results").mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="builder-routing.", dir=ROOT / "results") as path:
            result = subprocess.run(  # noqa: S603 -- Actual local block, inert shell mocks only.
                ["/bin/bash", "-c", mocks + "set -euo pipefail\ndriver_options=()\n" + selection],
                env={
                    "RUNNER_TEMP": path,
                    "ACT": act,
                    "LOCAL_CI_DISPOSABLE": "1",
                    "TEST_MACHINE": machine,
                    "TEST_IMAGE_ID": "sha256:" + "a" * 64,
                    "TEST_IMAGE_ARCH": "linux/arm64",
                    "TEST_BUILD_STATUS": "0",
                    **(overrides or {}),
                },
                capture_output=True,
                text=True,
                check=False,
                timeout=5,
            )
            calls = Path(path) / "docker-calls"
            return result, calls.read_text().splitlines() if calls.exists() else []

    def test_local_arm_builder_changes_only_the_disposable_native_driver_image(self) -> None:
        """Hosted and local AMD64 keep their driver; ARM uses a validated immutable image."""
        for machine, act, derived in (
            ("aarch64", "true", True),
            ("x86_64", "true", False),
            ("x86_64", "", False),
            ("aarch64", "", False),
        ):
            with self.subTest(machine=machine, act=act):
                result, calls = self.run_builder_selection(machine, act=act)
                self.assertEqual(result.returncode, 0, result.stderr)
                creates = [call for call in calls if call.startswith("buildx create ")]
                self.assertEqual(len(creates), 1)
                self.assertIn("--driver docker-container", creates[0])
                self.assertEqual("--driver-opt image=sha256:" in creates[0], derived)
                self.assertEqual(any(call.startswith("buildx build ") for call in calls), derived)
                if derived:
                    self.assertIn("--builder default --platform linux/arm64 --load", calls[0])
                    self.assertIn("image=sha256:" + "a" * 64, creates[0])

    def test_local_builder_rejects_missing_opt_in_failed_build_and_untrusted_image(self) -> None:
        """Failures cannot fall through to the bundled emulator or a mutable image tag."""
        cases = (
            {"LOCAL_CI_DISPOSABLE": ""},
            {"TEST_BUILD_STATUS": "42"},
            {"TEST_IMAGE_ID": "floating:tag"},
            {"TEST_IMAGE_ARCH": "linux/amd64"},
        )
        for options in cases:
            with self.subTest(options=options):
                result, calls = self.run_builder_selection("aarch64", overrides=options)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(any(call.startswith("buildx create ") for call in calls))
                if options.get("LOCAL_CI_DISPOSABLE") == "":
                    self.assertEqual(calls, [])

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
        self.assertNotIn("continue-on-error", release)

    def test_compose_is_verified_before_install_and_matches_both_execution_users(self) -> None:
        """Verify compose is verified before install and matches both execution users."""
        step = self.step(COMPOSE_TOOL)
        self.assertEqual(step["timeout-minutes"], "3")
        self.assertEqual(step["shell"], "bash")
        self.assertNotIn("continue-on-error", step)
        self.assertNotIn("if", step)
        command = string(step, "run")
        self.assertTrue(command.startswith("set -euo pipefail\n"))
        self.assertIn('mktemp -d "${RUNNER_TEMP}/simplestchat-compose.XXXXXXXX"', command)
        self.assertIn(
            "curl --disable --fail --silent --show-error --location --proto '=https' --tlsv1.2",
            command,
        )
        self.assertIn("--connect-timeout 10 --max-time 60", command)
        self.assertIn(
            "https://github.com/docker/compose/releases/download/v5.5.1/docker-compose-linux-${compose_arch}",
            command,
        )
        self.assertIn("db1889184726840f75c4f9c001048430d4f25b3be3cb084d3ddd762bc0aed576", command)
        self.assertIn('"${compose_download}/docker-compose" | sha256sum --check --strict', command)
        self.assertIn("sudo install -d -m 0755 /usr/local/lib/docker/cli-plugins", command)
        installation = (
            'sudo install -m 0755 "${compose_download}/docker-compose" '
            + "/usr/local/lib/docker/cli-plugins/docker-compose"
        )
        self.assertIn(installation, command)
        self.assertLess(command.index("sha256sum --check --strict"), command.index(installation))
        self.assertLess(
            command.index(installation), command.index("docker compose version --short")
        )
        self.assertIn(
            'test "$(timeout --signal=TERM --kill-after=2s 10s '
            + 'docker compose version --short)" = 5.5.1',
            command,
        )
        self.assertIn("sudo env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin LC_ALL=C", command)
        self.assertIn(
            "timeout --signal=TERM --kill-after=2s 10s /usr/bin/docker --host unix:///var/run/docker.sock",
            command,
        )
        self.assertEqual(command.count('compose version --short)" = 5.5.1'), 2)
        self.assertNotRegex(command, r"\b(?:apt|apt-get|systemctl|service|dockerd)\b")
        self.assertNotRegex(command, r"\b(?:latest|prune|remove|upgrade)\b")

    def test_compose_selects_an_authenticated_native_binary_before_any_download(self) -> None:
        """Exercise both native controllers and fail closed on unsupported machines."""
        command = string(self.step(COMPOSE_TOOL), "run")
        selection, separator, _ = command.partition("compose_download=")
        self.assertEqual(separator, "compose_download=")
        script = (
            'uname() { printf "%s\\n" "$TEST_MACHINE"; }\n'
            + selection
            + 'printf "%s %s\\n" "$compose_arch" "$compose_sha256"\n'
        )
        digests = {
            "x86_64": "db1889184726840f75c4f9c001048430d4f25b3be3cb084d3ddd762bc0aed576",
            "aarch64": "732e3a84c1a0f67256ce80bc2598a24546b10ca05f9faa97efceb1171ece2ef7",
        }
        for machine in ("x86_64", "aarch64", "arm64", "riscv64"):
            with self.subTest(machine=machine):
                result = subprocess.run(  # noqa: S603 -- Execute only the checked local selection block.
                    ["/bin/bash", "-c", script],
                    env={"TEST_MACHINE": machine},
                    capture_output=True,
                    text=True,
                    check=False,
                    timeout=5,
                )
                if machine == "riscv64":
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("Unsupported Compose controller architecture", result.stderr)
                    self.assertEqual(result.stdout, "")
                else:
                    architecture = "aarch64" if machine == "arm64" else machine
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(result.stdout, f"{architecture} {digests[architecture]}\n")

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
        self.assertEqual(integration["timeout-minutes"], "10")
        self.assertNotIn("continue-on-error", integration)
        self.assertNotIn("if", integration, "Do not silently skip a required release check")
        self.assertNotIn(
            "env", integration, "No deployment secrets or helper overrides enter the fixture"
        )

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

    def test_read_only_permissions_pinned_actions_and_no_remote_deployment(self) -> None:
        """Verify read only permissions pinned actions and no remote deployment."""
        self.assertEqual(self.workflow["permissions"], {"contents": "read"})
        self.assertNotIn("permissions", self.job)
        self.assertNotIn("environment", self.job)
        for step in self.steps:
            if "uses" in step:
                # First-party GitHub actions only, pinned by commit: checkout,
                # the Actions-cache layer store, and artifact retention.
                self.assertRegex(
                    string(step, "uses"),
                    r"^actions/(?:checkout|cache/(?:restore|save)|upload-artifact)@[a-f0-9]{40}$",
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
