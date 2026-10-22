"""Keep the full comparison owner inside the existing hosted workload contract."""

import unittest
from functools import cached_property
from pathlib import Path

from test_support import at, obj, objects, string, yaml_value

# isort: split
from release_json import JsonObject

ROOT = Path(__file__).resolve().parents[3]
NAME = "Build and compare the baseline and candidate"


class PerformanceCompareCiTests(unittest.TestCase):
    """The runtime port changes ownership, not profiles, setup or verdict gates."""

    @cached_property
    def job(self) -> JsonObject:
        """Read the informational hosted comparison without running a workload."""
        return obj(
            at(
                yaml_value(
                    (ROOT / ".github/workflows/performance.yml").read_text(),
                    scalars_as_strings=True,
                ),
                "jobs",
                "compare",
            )
        )

    @cached_property
    def steps(self) -> list[JsonObject]:
        """Keep only structured step mappings."""
        return objects(self.job["steps"])

    def test_same_hosted_setup_cache_and_ninety_minute_ceiling_precede_owner(self) -> None:
        """No new bootstrap, dependency or paid runner accompanies the owner."""
        self.assertEqual(self.job["runs-on"], "ubuntu-24.04")
        self.assertEqual(self.job["timeout-minutes"], "90")
        self.assertEqual(
            self.job["if"],
            "${{ !inputs.only || inputs.only == 'all' || inputs.only == 'compare' }}",
        )
        self.assertEqual(string(self.steps[0], "with", "fetch-depth"), "0")
        self.assertEqual(string(self.steps[0], "with", "persist-credentials"), "false")
        self.assertEqual(self.steps[1]["uses"], "./.github/actions/native-toolchain")
        self.assertEqual(string(self.steps[1], "with", "cache-key"), "rust-release-benchmark")
        self.assertEqual(string(self.steps[2], "with", "node-version"), "26.10.0")
        self.assertEqual(self.steps[3]["name"], "Install checksum-pinned npm")
        self.assertEqual(string(self.job, "env", "OPENSSL_STATIC"), "1")
        self.assertEqual(
            string(self.job, "env", "PIP_CONSTRAINT"),
            "${{ github.workspace }}/build/pip-constraints.txt",
        )

    def test_one_complete_owner_receives_only_the_existing_closed_profile_inputs(self) -> None:
        """All builds and six runs belong to one owner rather than split shells."""
        step = self.steps[4]
        self.assertEqual(step["name"], NAME)
        self.assertEqual(step["run"], "cargo run --locked -q -p xtask -- ci performance-compare")
        self.assertEqual(
            obj(step["env"]),
            {
                "BASELINE_REF": "${{ inputs.baseline || 'HEAD~1' }}",
                "CLIENTS": "${{ inputs.clients || '10' }}",
                "WORKERS": "${{ inputs.workers || '1' }}",
            },
        )
        for key in ("if", "continue-on-error", "timeout-minutes", "working-directory"):
            self.assertNotIn(key, step)
        self.assertEqual(len(self.steps), 7)

    def test_threshold_and_artifact_guards_paths_and_retention_stay_separate(self) -> None:
        """Failed completed comparisons still reach the unchanged result readers."""
        threshold, upload = self.steps[5:]
        self.assertEqual(threshold["name"], "Evaluate the regression budget")
        self.assertEqual(threshold["if"], "${{ !cancelled() }}")
        self.assertEqual(
            threshold["run"],
            "cargo run --locked -q -p xtask -- performance-thresholds "
            + '"${RUNNER_TEMP}/comparison/comparison.json"',
        )
        self.assertEqual(upload["name"], "Retain the comparison")
        self.assertEqual(upload["if"], "${{ !cancelled() }}")
        self.assertEqual(
            upload["uses"],
            "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a",
        )
        self.assertEqual(string(upload, "with", "path"), "${{ runner.temp }}/comparison")
        self.assertEqual(string(upload, "with", "if-no-files-found"), "warn")
        self.assertEqual(string(upload, "with", "retention-days"), "30")


if __name__ == "__main__":
    _ = unittest.main()
